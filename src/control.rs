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
use tracing::{debug, info, warn};
use webrtc::data_channel::{DataChannel, DataChannelEvent};

pub const LABEL: &str = "control";

const PING: &str = "ping";
const PONG: &str = "pong";
const BYE: &str = "bye";
const OPEN: &str = "open";
const OPEN_DATAGRAMS: &str = "open-datagrams";
/// The joiner asks for the host's world. Hosts that do not share one never answer.
const OPEN_WORLD: &str = "open-world";
const READY: &str = "ready";
const REFUSED: &str = "refused";

const PING_INTERVAL: Duration = Duration::from_secs(5);

/// How long a silent peer is waited for. Weak links and Wi-Fi hiccups drop out for tens of seconds
/// and come back on the same route, and riding that out is far cheaper than a new invite. A
/// Minecraft connection still times out after 30 seconds, but the tunnel survives for the player
/// to reconnect. A clean exit is still noticed at once, through the goodbye.
const PEER_TIMEOUT: Duration = Duration::from_secs(300);

/// Silence after which the ping is cleared and the outage logged. It only reports, never
/// disconnects.
const QUIET_AFTER: Duration = Duration::from_secs(15);

const BYE_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// Covers the host dialing the Minecraft server.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// How many stream IDs tunnel channels rotate through. Even IDs from 2 up; `control` is announced
/// with DCEP and takes the lowest free ID of the host's DTLS role, which is 0 or 1. libwebrtc, which
/// the Android app uses, negotiates only 1024 SCTP streams, so every ID has to stay below that.
const STREAM_ID_COUNT: u64 = 500;

/// The stream ID of tunnel connection `id`.
pub fn stream_id(id: u64) -> u16 {
    (2 + 2 * (id % STREAM_ID_COUNT)) as u16
}

pub fn channel_label(id: u64) -> String {
    format!("mc-{id}")
}

/// What a tunnel connection carries, which decides how the host reaches the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Stream,
    Datagrams,
    /// The world sync channel, see [`crate::sync`].
    World,
}

impl Transport {
    fn word(self) -> &'static str {
        match self {
            Self::Stream => OPEN,
            Self::Datagrams => OPEN_DATAGRAMS,
            Self::World => OPEN_WORLD,
        }
    }
}

/// Called on the host for each `open`. Must not block.
pub type OpenHandler = Arc<dyn Fn(u64, Transport) + Send + Sync>;

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
    pub async fn request(&self, control: &dyn DataChannel, id: u64, transport: Transport) -> bool {
        let (tx, rx) = oneshot::channel();
        self.0.lock().expect("pending opens lock").insert(id, tx);
        let accepted = async {
            let word = transport.word();
            control.send_text(&format!("{word} {id}")).await.ok()?;
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
            (Role::Host(on_open), OPEN) => on_open(id, Transport::Stream),
            (Role::Host(on_open), OPEN_DATAGRAMS) => on_open(id, Transport::Datagrams),
            (Role::Host(on_open), OPEN_WORLD) => on_open(id, Transport::World),
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
    let timing = Timing {
        ping_interval: PING_INTERVAL,
        quiet_after: QUIET_AFTER,
        peer_timeout: PEER_TIMEOUT,
    };
    run_with(channel, role, &mut shutdown, &rtt, timing).await
}

#[derive(Clone, Copy)]
struct Timing {
    ping_interval: Duration,
    quiet_after: Duration,
    peer_timeout: Duration,
}

async fn run_with(
    channel: Arc<dyn DataChannel>,
    role: Role,
    shutdown: &mut watch::Receiver<bool>,
    rtt: &watch::Sender<Option<Duration>>,
    timing: Timing,
) -> ControlEnd {
    let mut ticker = interval(timing.ping_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_heard = Instant::now();
    let mut ping_sent = None;
    let mut quiet = false;

    loop {
        tokio::select! {
            () = stop_requested(shutdown) => {
                say_goodbye(channel.as_ref()).await;
                return ControlEnd::Shutdown;
            }
            _ = ticker.tick() => {
                let silence = last_heard.elapsed();
                if silence > timing.peer_timeout {
                    return ControlEnd::TimedOut;
                }
                if silence > timing.quiet_after && !quiet {
                    quiet = true;
                    rtt.send_replace(None);
                    warn!(
                        "the other side went quiet, waiting up to {} minutes for it to come back",
                        timing.peer_timeout.as_secs() / 60
                    );
                }
                // Failing here only means the channel is not open yet.
                if channel.send_text(PING).await.is_ok() {
                    ping_sent = Some(Instant::now());
                }
            }
            event = channel.poll() => match event {
                Some(DataChannelEvent::OnMessage(message)) => {
                    last_heard = Instant::now();
                    if quiet {
                        quiet = false;
                        info!("the connection recovered");
                    }
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
        assert!((0..STREAM_ID_COUNT * 2).all(|id| stream_id(id) >= 2 && stream_id(id) < 1024));
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
        let role = Role::Host(Arc::new(move |id, transport| {
            recorded.lock().unwrap().push((id, transport))
        }));
        role.handle("open 3");
        role.handle("open-datagrams 6");
        role.handle("open-world 18446744073709551615");
        role.handle("ready 4");
        joiner_role().handle("open 5");
        let expected = [
            (3, Transport::Stream),
            (6, Transport::Datagrams),
            (u64::MAX, Transport::World),
        ];
        assert_eq!(*seen.lock().unwrap(), expected);
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
        let timing = Timing {
            ping_interval: beat,
            quiet_after: Duration::from_millis(200),
            peer_timeout: patience,
        };

        let remote_task = tokio::spawn(async move {
            run_with(remote, joiner_role(), &mut remote_stop, &no_rtt(), timing).await
        });
        let (rtt, measured) = watch::channel(None);
        let local_task = tokio::spawn(async move {
            run_with(local, joiner_role(), &mut local_stop, &rtt, timing).await
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
        // Going quiet clears the ping before the session is given up.
        sleep(Duration::from_millis(350)).await;
        assert!(!local_task.is_finished());
        assert!(
            measured.borrow().is_none(),
            "a quiet peer should clear the ping"
        );
        let end = timeout(Duration::from_secs(5), local_task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(end, ControlEnd::TimedOut);
        offerer.close().await;
        answerer.close().await;
    }

    /// Points every candidate in `sdp` at `address`.
    fn redirect_candidates(sdp: &str, address: std::net::SocketAddr) -> String {
        sdp.split_inclusive('\n')
            .map(|line| {
                if !line.starts_with("a=candidate:") {
                    return line.to_owned();
                }
                let mut fields: Vec<String> =
                    line.trim_end().split(' ').map(str::to_owned).collect();
                fields[4] = address.ip().to_string();
                fields[5] = address.port().to_string();
                fields.join(" ") + "\r\n"
            })
            .collect()
    }

    fn candidate_address(sdp: &str) -> std::net::SocketAddr {
        let line = sdp
            .lines()
            .find(|line| line.starts_with("a=candidate:"))
            .unwrap();
        let fields: Vec<_> = line.split(' ').collect();
        format!("{}:{}", fields[4], fields[5]).parse().unwrap()
    }

    /// Forwards UDP between two peers, dropping everything while `cut` is true. `a` reaches the
    /// first socket, which forwards to `b`; the reverse goes through the second.
    async fn cuttable_link(
        to_a: std::net::SocketAddr,
        to_b: watch::Receiver<Option<std::net::SocketAddr>>,
        cut: watch::Receiver<bool>,
    ) -> (std::net::SocketAddr, std::net::SocketAddr) {
        use tokio::net::UdpSocket;
        let toward_a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let toward_b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addresses = (
            toward_a.local_addr().unwrap(),
            toward_b.local_addr().unwrap(),
        );
        tokio::spawn(async move {
            let (mut from_b, mut from_a) = (vec![0; 2048], vec![0; 2048]);
            loop {
                tokio::select! {
                    Ok((read, _)) = toward_a.recv_from(&mut from_b) => {
                        let open = !*cut.borrow();
                        if open {
                            let _ = toward_b.send_to(&from_b[..read], to_a).await;
                        }
                    }
                    Ok((read, _)) = toward_b.recv_from(&mut from_a) => {
                        let (closed, to_b) = (*cut.borrow(), *to_b.borrow());
                        if let (false, Some(to_b)) = (closed, to_b) {
                            let _ = toward_a.send_to(&from_a[..read], to_b).await;
                        }
                    }
                }
            }
        });
        addresses
    }

    /// Runs with the real timings, so it takes over a minute: `cargo test -- --ignored`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn a_session_rides_out_a_40_second_outage() {
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

        // Each side only knows the relay, so every packet crosses it.
        let offer = offerer.create_offer().await.unwrap();
        let (cut, cut_rx) = watch::channel(false);
        let (set_answerer, answerer_address) = watch::channel(None);
        let (offerer_side, answerer_side) =
            cuttable_link(candidate_address(&offer), answerer_address, cut_rx).await;
        let answer = answerer
            .accept_offer(redirect_candidates(&offer, offerer_side))
            .await
            .unwrap();
        set_answerer.send_replace(Some(candidate_address(&answer)));
        offerer
            .accept_answer(redirect_candidates(&answer, answerer_side))
            .await
            .unwrap();
        offerer
            .wait_connected(Duration::from_secs(20))
            .await
            .unwrap();
        let remote = timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();

        let (_stop_local, local_stop) = watch::channel(false);
        let (_stop_remote, remote_stop) = watch::channel(false);
        let (rtt, mut measured) = watch::channel(None);
        let local_task = tokio::spawn(run(local, joiner_role(), local_stop, rtt));
        let remote_task = tokio::spawn(run(remote, joiner_role(), remote_stop, no_rtt()));
        timeout(Duration::from_secs(15), measured.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();

        cut.send_replace(true);
        sleep(Duration::from_secs(40)).await;
        assert!(
            measured.borrow().is_none(),
            "the outage should clear the ping"
        );
        cut.send_replace(false);

        timeout(Duration::from_secs(20), measured.wait_for(Option::is_some))
            .await
            .expect("pings should resume soon after the link comes back")
            .unwrap();
        assert!(!local_task.is_finished() && !remote_task.is_finished());
        local_task.abort();
        remote_task.abort();
        offerer.close().await;
        answerer.close().await;
    }
}
