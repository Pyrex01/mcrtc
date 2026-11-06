//! The `control` data channel: heartbeat, a clean goodbye, and setting up tunnel channels.
//!
//! A tunnel channel is announced here instead of through the usual in-band DCEP handshake. DCEP
//! makes the library pick the lowest free stream ID, and it frees an ID the moment `close()` is
//! called, while SCTP is still resetting the old stream. Reconnecting quickly, as server-list
//! pings do, then lands on that ID: the new channel silently never opens, or the late reset kills
//! it. Negotiated channels let the joiner pick the ID, so IDs simply rotate through a range far
//! larger than the number of connections that can overlap with a reset.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use tokio::time::{interval, sleep, timeout, Instant, MissedTickBehavior};
use tracing::debug;
use webrtc::data_channel::{DataChannel, DataChannelEvent};

pub const LABEL: &str = "control";

const PING: &str = "ping";
const PONG: &str = "pong";
const BYE: &str = "bye";
const OPEN: &str = "open";
const READY: &str = "ready";
const REFUSED: &str = "refused";

const PING_INTERVAL: Duration = Duration::from_secs(5);

/// Longer than the bridge's write stall timeout, since a stalled stream delays this channel too.
const PEER_TIMEOUT: Duration = Duration::from_secs(45);

const BYE_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// Covers the host dialing the Minecraft server.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// How many stream IDs tunnel channels rotate through. Even IDs from 2 up; `control` is announced
/// with DCEP and takes the lowest free ID of the host's DTLS role, which is 0 or 1.
const STREAM_ID_COUNT: u64 = 30_000;

/// The stream ID of tunnel connection `id`.
pub fn stream_id(id: u64) -> u16 {
    (2 + 2 * (id % STREAM_ID_COUNT)) as u16
}

pub fn channel_label(id: u64) -> String {
    format!("mc-{id}")
}

/// Called on the host for each `open`. Must not block.
pub type OpenHandler = Arc<dyn Fn(u64) + Send + Sync>;

/// The joiner's open requests still waiting for an answer.
#[derive(Clone, Default)]
pub struct PendingOpens(Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>);

impl PendingOpens {
    fn resolve(&self, id: u64, accepted: bool) {
        let waiter = self.0.lock().expect("pending opens lock").remove(&id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(accepted);
        }
    }

    /// Asks the host to open its end of tunnel connection `id`. False means no.
    pub async fn request(&self, control: &dyn DataChannel, id: u64) -> bool {
        let (tx, rx) = oneshot::channel();
        self.0.lock().expect("pending opens lock").insert(id, tx);
        let accepted = async {
            control.send_text(&format!("{OPEN} {id}")).await.ok()?;
            rx.await.ok()
        };
        let answer = timeout(OPEN_TIMEOUT, accepted).await.ok().flatten();
        self.0.lock().expect("pending opens lock").remove(&id);
        answer.unwrap_or(false)
    }
}

/// The host's answer to an `open`.
pub async fn answer_open(control: &dyn DataChannel, id: u64, accepted: bool) {
    let word = if accepted { READY } else { REFUSED };
    let _ = control.send_text(&format!("{word} {id}")).await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEnd {
    /// The other side said goodbye.
    PeerLeft,
    /// The channel closed without a goodbye.
    Closed,
    /// The other side went silent.
    TimedOut,
    /// We are shutting down and said goodbye.
    Shutdown,
}

/// What this side does with tunnel messages.
#[derive(Clone)]
pub enum Role {
    Host(OpenHandler),
    Joiner(PendingOpens),
}

impl Role {
    fn handle(&self, message: &str) {
        let Some((word, id)) = message.split_once(' ') else {
            debug!(len = message.len(), "ignoring unknown control message");
            return;
        };
        let Ok(id) = id.parse() else { return };
        match (self, word) {
            (Role::Host(on_open), OPEN) => on_open(id),
            (Role::Joiner(pending), READY) => pending.resolve(id, true),
            (Role::Joiner(pending), REFUSED) => pending.resolve(id, false),
            _ => debug!(word, "ignoring unexpected control message"),
        }
    }
}

/// Serves the control channel until the session ends. Says goodbye when `shutdown` turns true.
pub async fn run(
    channel: Arc<dyn DataChannel>,
    role: Role,
    mut shutdown: watch::Receiver<bool>,
    rtt: watch::Sender<Option<Duration>>,
) -> ControlEnd {
    run_with(
        channel,
        role,
        &mut shutdown,
        &rtt,
        PING_INTERVAL,
        PEER_TIMEOUT,
    )
    .await
}

async fn run_with(
    channel: Arc<dyn DataChannel>,
    role: Role,
    shutdown: &mut watch::Receiver<bool>,
    rtt: &watch::Sender<Option<Duration>>,
    ping_interval: Duration,
    peer_timeout: Duration,
) -> ControlEnd {
    let mut ticker = interval(ping_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_heard = Instant::now();
    let mut ping_sent = None;

    loop {
        tokio::select! {
            () = stop_requested(shutdown) => {
                say_goodbye(channel.as_ref()).await;
                return ControlEnd::Shutdown;
            }
            _ = ticker.tick() => {
                if last_heard.elapsed() > peer_timeout {
                    return ControlEnd::TimedOut;
                }
                // Failing here only means the channel is not open yet.
                if channel.send_text(PING).await.is_ok() {
                    ping_sent = Some(Instant::now());
                }
            }
            event = channel.poll() => match event {
                Some(DataChannelEvent::OnMessage(message)) => {
                    last_heard = Instant::now();
                    match message.data.as_ref() {
                        data if data == PING.as_bytes() => {
                            let _ = channel.send_text(PONG).await;
                        }
                        data if data == BYE.as_bytes() => return ControlEnd::PeerLeft,
                        data if data == PONG.as_bytes() => {
                            if let Some(sent) = ping_sent.take() {
                                rtt.send_replace(Some(sent.elapsed()));
                            }
                        }
                        data => role.handle(&String::from_utf8_lossy(data)),
                    }
                }
                Some(DataChannelEvent::OnClose) | None => return ControlEnd::Closed,
                Some(_) => {}
            }
        }
    }
}

/// Resolves when `shutdown` turns true.
pub async fn stop_requested(shutdown: &mut watch::Receiver<bool>) {
    if shutdown.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn say_goodbye(channel: &dyn DataChannel) {
    if channel.send_text(BYE).await.is_err() {
        return;
    }
    let delivered = async {
        while channel
            .outstanding_bytes()
            .await
            .is_ok_and(|bytes| bytes > 0)
        {
            sleep(Duration::from_millis(10)).await;
        }
    };
    let _ = timeout(BYE_FLUSH_TIMEOUT, delivered).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::test_support::*;
    use crate::peer::{Peer, PeerOptions};
    use tokio::sync::mpsc;

    fn no_rtt() -> watch::Sender<Option<Duration>> {
        watch::channel(None).0
    }

    fn joiner_role() -> Role {
        Role::Joiner(PendingOpens::default())
    }

    #[test]
    fn stream_ids_are_valid_and_rotate_slowly() {
        assert_eq!(stream_id(0), 2);
        assert_eq!(stream_id(1), 4);
        assert_eq!(stream_id(STREAM_ID_COUNT), stream_id(0));
        assert!((0..STREAM_ID_COUNT * 2).all(|id| stream_id(id) >= 2 && stream_id(id) < 60_004));
    }

    #[test]
    fn answers_resolve_only_the_matching_request() {
        let pending = PendingOpens::default();
        let (tx, mut rx) = oneshot::channel();
        pending.0.lock().unwrap().insert(7, tx);
        let role = Role::Joiner(pending);

        role.handle("ready 8");
        role.handle("ready x");
        role.handle("nonsense");
        assert!(rx.try_recv().is_err());
        role.handle("refused 7");
        assert_eq!(rx.try_recv(), Ok(false));
    }

    #[test]
    fn only_the_host_reacts_to_open() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let role = Role::Host(Arc::new(move |id| recorded.lock().unwrap().push(id)));
        role.handle("open 3");
        role.handle("ready 4");
        joiner_role().handle("open 5");
        assert_eq!(*seen.lock().unwrap(), [3]);
    }

    async fn connected_pair() -> (Peer, Peer, Arc<dyn DataChannel>, Arc<dyn DataChannel>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let offerer = Peer::new(PeerOptions::loopback(), ignore_channels())
            .await
            .unwrap();
        let answerer = Peer::new(
            PeerOptions::loopback(),
            Arc::new(move |channel| {
                let _ = tx.send(channel);
            }),
        )
        .await
        .unwrap();
        let local = offerer.create_channel(LABEL).await.unwrap();
        connect_in_memory(&offerer, &answerer).await.unwrap();
        let remote = timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        (offerer, answerer, local, remote)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn goodbye_reaches_the_other_side() {
        let (offerer, answerer, local, remote) = connected_pair().await;
        let (stop_local, local_stop) = watch::channel(false);
        let (_stop_remote, remote_stop) = watch::channel(false);
        let local_task = tokio::spawn(run(local, joiner_role(), local_stop, no_rtt()));
        let remote_task = tokio::spawn(run(remote, joiner_role(), remote_stop, no_rtt()));

        sleep(Duration::from_millis(200)).await;
        stop_local.send_replace(true);

        let limit = Duration::from_secs(5);
        assert_eq!(
            timeout(limit, local_task).await.unwrap().unwrap(),
            ControlEnd::Shutdown
        );
        assert_eq!(
            timeout(limit, remote_task).await.unwrap().unwrap(),
            ControlEnd::PeerLeft
        );
        offerer.close().await;
        answerer.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heartbeat_keeps_a_live_session_going_and_detects_a_silent_one() {
        let (offerer, answerer, local, remote) = connected_pair().await;
        let (_stop, mut local_stop) = watch::channel(false);
        let (stop_remote, mut remote_stop) = watch::channel(false);
        let beat = Duration::from_millis(50);
        let patience = Duration::from_millis(500);

        let remote_task = tokio::spawn(async move {
            run_with(
                remote,
                joiner_role(),
                &mut remote_stop,
                &no_rtt(),
                beat,
                patience,
            )
            .await
        });
        let (rtt, measured) = watch::channel(None);
        let local_task = tokio::spawn(async move {
            run_with(local, joiner_role(), &mut local_stop, &rtt, beat, patience).await
        });

        // Several multiples of the timeout pass without either side giving up.
        sleep(patience * 3).await;
        assert!(!local_task.is_finished());
        assert!(!remote_task.is_finished());
        assert!(
            measured.borrow().is_some(),
            "the heartbeat should measure the round trip"
        );

        // Stop the remote side's answers without a goodbye reaching anyone.
        remote_task.abort();
        drop(stop_remote);
        let end = timeout(Duration::from_secs(5), local_task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(end, ControlEnd::TimedOut);
        offerer.close().await;
        answerer.close().await;
    }
}
