//! The shared UDP <-> data channel relay, Bedrock's counterpart to [`crate::bridge`].
//!
//! RakNet does its own acknowledgements, resends and ordering, so a flow rides an unordered data
//! channel without retransmits: a lost datagram stays lost and RakNet deals with it, exactly as
//! on a real network. UDP has no connections, so each client address is treated as one, and a
//! flow ends after a stretch of silence.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use tokio::net::{lookup_host, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::{sleep, Instant};
use tracing::{debug, warn};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};

use crate::bridge::BUFFERED_LOW;

pub const MAX_DATAGRAM: usize = 65_535;

/// Far longer than RakNet stays quiet in a live session, which pings several times a second.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

const UNCONNECTED_PONG: u8 = 0x1c;

/// Every offline RakNet message carries this, which tells a real pong from a lookalike.
const RAKNET_MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

/// A pong is its ID, a timestamp, the server's GUID, the magic, then a length and the status text.
const PONG_GUID_AT: usize = 9;
const PONG_MAGIC_AT: usize = 17;
const PONG_LENGTH_AT: usize = PONG_MAGIC_AT + RAKNET_MAGIC.len();
const PONG_TEXT_AT: usize = PONG_LENGTH_AT + 2;

/// `MCPE;motd;protocol;version;players;max;guid;level;mode;mode id;port;IPv6 port;...`, and how
/// many fields follow the ports depends on the version.
const STATUS_GUID_FIELD: usize = 6;
const STATUS_PORT_FIELDS: [usize; 2] = [10, 11];

/// The Minecraft end of a flow.
pub enum Endpoint {
    /// The host's own socket to the server, connected so only the server can answer.
    Server {
        socket: UdpSocket,
        buffer: Vec<u8>,
        warned: bool,
    },
    /// A friend's game behind the join socket, which every client shares.
    Client {
        socket: Arc<UdpSocket>,
        address: SocketAddr,
        inbound: mpsc::Receiver<BytesMut>,
        port: u16,
    },
}

impl Endpoint {
    pub fn server(socket: UdpSocket) -> Self {
        Self::Server {
            socket,
            buffer: vec![0; MAX_DATAGRAM],
            warned: false,
        }
    }

    pub fn client(
        socket: Arc<UdpSocket>,
        address: SocketAddr,
        inbound: mpsc::Receiver<BytesMut>,
        port: u16,
    ) -> Self {
        Self::Client {
            socket,
            address,
            inbound,
            port,
        }
    }

    /// The next datagram for the tunnel. `None` means this end is gone.
    async fn recv(&mut self) -> Option<BytesMut> {
        match self {
            Self::Client { inbound, .. } => inbound.recv().await,
            Self::Server {
                socket,
                buffer,
                warned,
            } => loop {
                match socket.recv(buffer).await {
                    Ok(read) => return Some(BytesMut::from(&buffer[..read])),
                    // Only an earlier send bouncing, and the server may just not be up yet.
                    Err(error) if is_bounce(&error) => {
                        if !std::mem::replace(warned, true) {
                            warn!(%error, "could not reach the Minecraft server");
                        }
                    }
                    Err(error) => {
                        warn!(%error, "lost the socket to the Minecraft server");
                        return None;
                    }
                }
            },
        }
    }

    /// Hands a datagram from the tunnel to Minecraft.
    async fn send(&self, data: &[u8]) -> io::Result<()> {
        match self {
            Self::Server { socket, .. } => socket.send(data).await.map(drop),
            Self::Client {
                socket,
                address,
                port,
                ..
            } => match advertise_port(data, *port) {
                Some(pong) => socket.send_to(&pong, address).await.map(drop),
                None => socket.send_to(data, address).await.map(drop),
            },
        }
    }
}

fn is_bounce(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset
    )
}

/// A socket that only talks to `server`. No packets are sent, so this cannot tell if it is up.
pub async fn dial(server: &str) -> io::Result<UdpSocket> {
    let address = lookup_host(server)
        .await?
        .next()
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "the address resolved to nothing"))?;
    let local: SocketAddr = match address {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = UdpSocket::bind(local).await?;
    socket.connect(address).await?;
    Ok(socket)
}

/// Rewrites the ports in a server's status pong to `port`, or `None` if `data` is not a pong.
///
/// The game can connect to the port a pong advertises rather than the one it pinged. That is the
/// server's own port, which means nothing on the friend's machine unless it matches the port
/// mcrtc listens on, and following it would miss the tunnel.
fn advertise_port(data: &[u8], port: u16) -> Option<Vec<u8>> {
    if *data.first()? != UNCONNECTED_PONG
        || data.get(PONG_MAGIC_AT..PONG_LENGTH_AT)? != RAKNET_MAGIC
    {
        return None;
    }
    let length = data.get(PONG_LENGTH_AT..PONG_TEXT_AT)?;
    let end = PONG_TEXT_AT + u16::from_be_bytes([length[0], length[1]]) as usize;
    let status = std::str::from_utf8(data.get(PONG_TEXT_AT..end)?).ok()?;

    // A `;` in the server's name moves every field, and the GUID in the header exposes that.
    let guid = u64::from_be_bytes(data[PONG_GUID_AT..PONG_MAGIC_AT].try_into().ok()?);
    let mut fields: Vec<_> = status.split(';').collect();
    if *fields.get(STATUS_GUID_FIELD)? != guid.to_string() {
        return None;
    }
    let port = port.to_string();
    for index in STATUS_PORT_FIELDS {
        fields.get(index)?.parse::<u16>().ok()?;
        fields[index] = &port;
    }
    let status = fields.join(";");

    let mut pong = data[..PONG_LENGTH_AT].to_vec();
    pong.extend_from_slice(&u16::try_from(status.len()).ok()?.to_be_bytes());
    pong.extend_from_slice(status.as_bytes());
    pong.extend_from_slice(&data[end..]);
    Some(pong)
}

/// Sends unless too much is already queued. Waiting would only hand RakNet stale data, so a
/// congested link drops datagrams the way any other network would.
async fn forward(channel: &dyn DataChannel, data: BytesMut) {
    match channel.outstanding_bytes().await {
        Ok(queued) if queued <= BUFFERED_LOW => {
            if let Err(error) = channel.send(data).await {
                debug!(%error, "dropped a datagram");
            }
        }
        _ => debug!("dropped a datagram, the channel is congested"),
    }
}

/// Moves datagrams both ways until the flow goes quiet or either side closes.
pub async fn relay(end: Endpoint, channel: Arc<dyn DataChannel>) {
    relay_with(end, channel, IDLE_TIMEOUT).await;
}

async fn relay_with(mut end: Endpoint, channel: Arc<dyn DataChannel>, idle_timeout: Duration) {
    let mut open = matches!(channel.ready_state().await, Ok(RTCDataChannelState::Open));
    let idle = sleep(idle_timeout);
    tokio::pin!(idle);

    loop {
        tokio::select! {
            () = &mut idle => {
                debug!("flow went quiet");
                break;
            }
            data = end.recv(), if open => {
                let Some(data) = data else { break };
                idle.as_mut().reset(Instant::now() + idle_timeout);
                forward(channel.as_ref(), data).await;
            }
            event = channel.poll() => match event {
                Some(DataChannelEvent::OnOpen) => open = true,
                Some(DataChannelEvent::OnMessage(message)) => {
                    idle.as_mut().reset(Instant::now() + idle_timeout);
                    if let Err(error) = end.send(&message.data).await {
                        debug!(%error, "could not hand a datagram to Minecraft");
                    }
                }
                Some(DataChannelEvent::OnClose) | None => break,
                Some(_) => {}
            }
        }
    }

    if let Err(error) = channel.close().await {
        debug!(%error, "data channel was already closed");
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    /// The format of a real 1.26 dedicated server, with the GUID [`pong`] puts in the header.
    pub const STATUS: &str = "MCPE;A Server;2225;1.26.62;0;10;506381209866536711;Bedrock level;\
                              Survival;1;19150;19151;0;0;1;";

    /// What a dedicated server answers a ping with.
    pub fn pong(status: &str) -> Vec<u8> {
        let mut pong = vec![UNCONNECTED_PONG];
        pong.extend_from_slice(&[7; 16]); // The timestamp and the GUID.
        pong.extend_from_slice(&RAKNET_MAGIC);
        pong.extend_from_slice(&(status.len() as u16).to_be_bytes());
        pong.extend_from_slice(status.as_bytes());
        pong
    }

    pub fn pong_status(pong: &[u8]) -> &str {
        std::str::from_utf8(&pong[PONG_TEXT_AT..]).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::peer::test_support::*;
    use crate::peer::{Peer, PeerOptions};
    use tokio::time::timeout;

    #[test]
    fn pongs_advertise_the_local_port() {
        let rewritten = advertise_port(&pong(STATUS), 19132).unwrap();
        let expected = STATUS.replace("19150;19151", "19132;19132");
        assert_eq!(pong_status(&rewritten), expected);
        assert_eq!(rewritten, pong(&expected));
    }

    #[test]
    fn a_shorter_port_keeps_the_length_right() {
        let rewritten = advertise_port(&pong(STATUS), 80).unwrap();
        assert_eq!(rewritten, pong(&STATUS.replace("19150;19151", "80;80")));
    }

    #[test]
    fn a_semicolon_in_the_name_leaves_the_pong_alone() {
        let status = STATUS.replace("A Server", "Semi;colon");
        assert_eq!(advertise_port(&pong(&status), 19132), None);
    }

    #[test]
    fn older_pongs_that_end_at_the_ports_work_too() {
        let status = STATUS.strip_suffix("0;0;1;").unwrap();
        let rewritten = advertise_port(&pong(status), 19132).unwrap();
        assert_eq!(
            rewritten,
            pong(&status.replace("19150;19151", "19132;19132"))
        );
    }

    #[test]
    fn anything_but_a_well_formed_pong_passes_through() {
        assert_eq!(advertise_port(&[], 1), None);
        assert_eq!(advertise_port(&[0x84, 1, 2, 3], 1), None);

        let mut no_magic = pong(STATUS);
        no_magic[PONG_MAGIC_AT] = 0xaa;
        assert_eq!(advertise_port(&no_magic, 1), None);

        let full = pong(STATUS);
        assert_eq!(advertise_port(&full[..full.len() - 1], 1), None);
        assert_eq!(advertise_port(&full[..PONG_LENGTH_AT], 1), None);

        assert_eq!(advertise_port(&pong("MCPE;too;short"), 1), None);
        let no_ports = STATUS.replace("19150;19151;", "x;y;");
        assert_eq!(advertise_port(&pong(&no_ports), 1), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_quiet_flow_closes_its_channel_on_both_sides() {
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
        let local = offerer.create_channel("flow").await.unwrap();
        connect_in_memory(&offerer, &answerer).await.unwrap();
        let limit = Duration::from_secs(10);
        let remote = timeout(limit, rx.recv()).await.unwrap().unwrap();

        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let end = dial(&server.local_addr().unwrap().to_string())
            .await
            .unwrap();
        let quiet = relay_with(Endpoint::server(end), local, Duration::from_millis(200));
        let (_keep_open, inbound) = mpsc::channel(1);
        let client = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let address = client.local_addr().unwrap();
        let other_side = relay(Endpoint::client(client, address, inbound, 1), remote);

        timeout(limit, async { tokio::join!(quiet, other_side) })
            .await
            .unwrap();
        offerer.close().await;
        answerer.close().await;
    }
}
