//! Join mode: the local TCP listener, one data channel per connection.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, timeout, Instant};
use tracing::{debug, warn};
use webrtc::data_channel::DataChannel;

use crate::bridge::{bridge, Counter};
use crate::console::Console;
use crate::control::{self, channel_label, stream_id, ControlEnd, PendingOpens, Role};
use crate::events::{Event, Reporter};
use crate::host::report_stats;
use crate::invite::{Payload, PayloadType, DEFAULT_TTL};
use crate::peer::{certificate_id, generate_certificate, ChannelHandler, Peer, PeerOptions};

/// The host needs time to receive and paste the reply, so this is as long as a reply stays valid.
const CONNECT_TIMEOUT: Duration = DEFAULT_TTL;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct JoinOptions {
    pub peer: PeerOptions,
    pub listen: String,
    pub raw_json: bool,
    pub reporter: Reporter,
}

/// The join side of an established session.
#[derive(Clone)]
pub struct Tunnel {
    pub peer: Peer,
    pub control: Arc<dyn DataChannel>,
    pub pending: PendingOpens,
    pub connections: Counter,
}

impl Tunnel {
    /// Accepts TCP connections until `shutdown`, bridging each over a new channel. The bridges
    /// live on until their connection or the peer connection ends.
    pub async fn serve(self, listener: TcpListener, mut shutdown: watch::Receiver<bool>) {
        let mut next_id = 0;
        loop {
            let accepted = tokio::select! {
                () = control::stop_requested(&mut shutdown) => return,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, address)) => {
                    debug!(%address, "accepted a connection");
                    tokio::spawn(self.clone().connect(stream, next_id));
                    next_id += 1;
                }
                Err(error) => {
                    warn!(%error, "could not accept a connection");
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    async fn connect(self, stream: TcpStream, id: u64) {
        let _slot = self.connections.acquire();
        let label = channel_label(id);
        let channel = match self
            .peer
            .create_negotiated_channel(&label, stream_id(id))
            .await
        {
            Ok(channel) => channel,
            Err(error) => {
                warn!("{error:#}");
                return;
            }
        };
        if self.pending.request(self.control.as_ref(), id).await {
            bridge(stream, channel).await;
        } else {
            warn!(channel = %label, "the host refused a connection");
            let _ = channel.close().await;
        }
        debug!(channel = %label, "connection closed");
    }
}

/// Hands the host's `control` channel to `control` and refuses anything else.
fn control_handler(control: mpsc::Sender<Arc<dyn DataChannel>>) -> ChannelHandler {
    Arc::new(move |channel| {
        let control = control.clone();
        tokio::spawn(async move {
            if channel
                .label()
                .await
                .is_ok_and(|label| label == control::LABEL)
            {
                let _ = control.try_send(channel);
            } else {
                warn!("refusing an unknown data channel from the host");
                let _ = channel.close().await;
            }
        });
    })
}

pub async fn run(
    mut options: JoinOptions,
    mut console: Console,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let reporter = options.reporter.clone();
    // Bind first: finding out the port is taken is better before the invite dance than after it.
    let listener = TcpListener::bind(&options.listen).await.with_context(|| {
        format!(
            "could not listen on {0}. Is something already using it? If the Minecraft server \
             runs on this machine, pick another port with --listen 127.0.0.1:25566",
            options.listen
        )
    })?;

    let certificate = match options.peer.certificate.take() {
        Some(certificate) => certificate,
        None => generate_certificate()?,
    };
    reporter.report(Event::YourId {
        id: certificate_id(&certificate)?,
    });
    options.peer.certificate = Some(certificate);

    let Some(sdp) = read_invite(&mut console, &mut shutdown, &reporter).await else {
        return Ok(());
    };

    let (control_tx, mut control_rx) = mpsc::channel(1);
    let peer = Peer::new(options.peer, control_handler(control_tx)).await?;
    let session = async {
        reporter.report(Event::Gathering);
        let sdp = peer.accept_offer(sdp).await?;
        let payload = Payload::new(PayloadType::Answer, sdp, DEFAULT_TTL);
        let reply = if options.raw_json {
            payload.encode_raw_json()?
        } else {
            payload.encode()?
        };
        reporter.report(Event::Reply { text: reply });
        reporter.report(Event::WaitingForHost);

        peer.wait_connected(CONNECT_TIMEOUT).await?;
        let route = peer.log_selected_pair("host").await;
        let control = timeout(CONTROL_TIMEOUT, control_rx.recv())
            .await
            .ok()
            .flatten()
            .ok_or_else(|| anyhow!("the host never opened its control channel"))?;
        Ok::<_, anyhow::Error>((control, route))
    };

    let (control, route) = tokio::select! {
        () = control::stop_requested(&mut shutdown) => {
            peer.close().await;
            return Ok(());
        }
        session = session => match session {
            Ok(session) => session,
            Err(error) => {
                peer.close().await;
                return Err(error);
            }
        },
    };

    let address = listener.local_addr()?.to_string();
    reporter.report(Event::Joined { address, route });
    let connections = Counter::default();
    let pending = PendingOpens::default();
    let tunnel = Tunnel {
        peer: peer.clone(),
        control: control.clone(),
        pending: pending.clone(),
        connections: connections.clone(),
    };
    let (stop_serving, serving) = watch::channel(false);
    let server = tokio::spawn(tunnel.serve(listener, serving));

    let (rtt, measured) = watch::channel(None);
    let stats = report_stats(
        "host".to_owned(),
        connections.clone(),
        measured,
        reporter.clone(),
    );
    let end = tokio::select! {
        end = control::run(control, Role::Joiner(pending), shutdown, rtt) => end,
        state = peer.wait_lost() => {
            debug!(%state, "connection lost");
            ControlEnd::Closed
        }
        () = stats => ControlEnd::Closed,
    };

    stop_serving.send_replace(true);
    let _ = server.await;
    peer.close().await;
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    while connections.get() > 0 && Instant::now() < deadline {
        sleep(Duration::from_millis(10)).await;
    }

    match end {
        ControlEnd::PeerLeft => reporter.report(Event::Left {
            reason: "The host closed the connection.".to_owned(),
        }),
        ControlEnd::Shutdown => reporter.report(Event::Left {
            reason: "Disconnected.".to_owned(),
        }),
        ControlEnd::Closed => bail!("the connection to the host was lost"),
        ControlEnd::TimedOut => bail!("the host stopped responding"),
    }
    Ok(())
}

/// Asks until a valid invite arrives. `None` means stop.
async fn read_invite(
    console: &mut Console,
    shutdown: &mut watch::Receiver<bool>,
    reporter: &Reporter,
) -> Option<String> {
    loop {
        reporter.report(Event::AskInvite);
        let paste = tokio::select! {
            () = control::stop_requested(shutdown) => return None,
            paste = console.read_paste() => paste?,
        };
        match Payload::parse(&paste, PayloadType::Offer) {
            Ok(payload) => return Some(payload.sdp),
            Err(error) => reporter.report(Event::BadPaste {
                error: format!("{error:#}"),
            }),
        }
    }
}
