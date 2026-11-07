//! Carries a Bedrock game's NetherNet connection through the tunnel.
//!
//! A NetherNet join has two parts. First the game signals over HTTP on the server's TCP port:
//! `GET /v1/join` for the server's details, then `POST /v1/join/{id}` with a WebRTC offer, which
//! the server answers. mcrtc already carries that like any TCP connection. Then the game opens its
//! own WebRTC connection over UDP to an address in the answer, which would go straight to the
//! server and miss the tunnel. The game also ignores loopback addresses, so the server cannot
//! simply be told to advertise `127.0.0.1`.
//!
//! So the join side rewrites the signaling as it passes. In the answer, the server's addresses are
//! replaced by one of this machine's own (not loopback), where a relay socket waits and hands the
//! game's datagrams to the tunnel like any other UDP flow. In the offer, the game's addresses are
//! replaced by one nothing can reach, so the server can only talk to the game through the tunnel,
//! and the game's addresses never leave this machine. Fingerprints, credentials and the server's
//! identity are left alone, so the game still checks it is talking to the real server.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// Where the server is told the game is. TEST-NET-1 is reserved and routed nowhere.
const PLACEHOLDER_CANDIDATE: &str = "a=candidate:mcrtc 1 udp 1 192.0.2.1 9 typ host";

const CANDIDATE: &str = "a=candidate:";

/// A signaling message head bigger than this is not what we expect, so it is passed through.
const MAX_HEAD: usize = 64 * 1024;

/// SDP is a few kilobytes. Anything bigger is passed through untouched.
const MAX_BODY: usize = 1024 * 1024;

const READ_CHUNK: usize = 16 * 1024;

const METHODS: [&str; 8] = [
    "GET ", "POST ", "PUT ", "HEAD ", "DELETE ", "OPTIONS ", "PATCH ", "CONNECT ",
];

/// The join side's relay socket, as the game is told about it.
pub struct Relay {
    /// The address put in the answer. The relay socket listens here.
    pub candidate: SocketAddr,
    /// The game's own addresses, from its offers. Only these (and loopback) may open flows on the
    /// relay socket, which listens on a real interface.
    allowed: Mutex<HashSet<IpAddr>>,
}

impl Relay {
    pub fn new(candidate: SocketAddr) -> Self {
        Self {
            candidate,
            allowed: Mutex::default(),
        }
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        ip.is_loopback() || self.allowed.lock().expect("relay lock").contains(&ip)
    }

    fn allow(&self, ips: impl IntoIterator<Item = IpAddr>) {
        self.allowed.lock().expect("relay lock").extend(ips);
    }
}

/// An address of this machine that the game can send to, which is never loopback: the one the
/// default route leaves from. Finding it sends no packets.
pub fn local_address() -> Option<IpAddr> {
    let probe = |bind: &str, target: &str| {
        let socket = UdpSocket::bind(bind).ok()?;
        socket.connect(target).ok()?;
        let ip = socket.local_addr().ok()?.ip();
        (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
    };
    probe("0.0.0.0:0", "8.8.8.8:53").or_else(|| probe("[::]:0", "[2001:4860:4860::8888]:53"))
}

/// The address and port of an SDP candidate line, when it has a literal IP.
fn candidate_address(line: &str) -> Option<SocketAddr> {
    let mut fields = line.strip_prefix(CANDIDATE)?.split_whitespace();
    let ip = fields.nth(4)?.parse::<IpAddr>().ok()?;
    let port = fields.next()?.parse::<u16>().ok()?;
    Some(SocketAddr::new(ip, port))
}

/// Replaces every candidate line with `replacement`, keeping everything else byte for byte.
/// Returns the new SDP and the addresses of the lines it removed.
fn replace_candidates(sdp: &str, replacement: &str) -> (String, Vec<SocketAddr>) {
    let mut out = String::with_capacity(sdp.len());
    let mut removed = Vec::new();
    let mut replaced = false;
    for line in sdp.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if !content.starts_with(CANDIDATE) {
            out.push_str(line);
            continue;
        }
        removed.extend(candidate_address(content));
        if !replaced {
            replaced = true;
            out.push_str(replacement);
            out.push_str(&line[content.len()..]);
        }
    }
    if !replaced {
        // No candidates at all: add ours at the end, so there is still a way in.
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str("\r\n");
        }
        out.push_str(replacement);
        out.push_str("\r\n");
    }
    (out, removed)
}

/// The game's offer: its addresses are swapped for the placeholder and returned.
pub fn rewrite_offer(sdp: &str) -> (String, Vec<IpAddr>) {
    let (sdp, removed) = replace_candidates(sdp, PLACEHOLDER_CANDIDATE);
    (
        sdp,
        removed.into_iter().map(|address| address.ip()).collect(),
    )
}

/// The server's answer: its addresses are swapped for the relay's. Returns the server's own.
pub fn rewrite_answer(sdp: &str, relay: SocketAddr) -> (String, Vec<SocketAddr>) {
    let line = format!(
        "{CANDIDATE}mcrtc 1 udp 2130706431 {} {} typ host",
        relay.ip(),
        relay.port()
    );
    replace_candidates(sdp, &line)
}

/// One HTTP/1.1 message head, as received.
struct Head {
    raw: Vec<u8>,
    first_line: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn parse(raw: Vec<u8>) -> Option<Self> {
        let text = std::str::from_utf8(&raw).ok()?;
        let mut lines = text.split("\r\n");
        let first_line = lines.next()?.to_owned();
        let headers = lines
            .filter(|line| !line.is_empty())
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        Some(Self {
            raw,
            first_line,
            headers,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn chunked(&self) -> bool {
        self.header("transfer-encoding")
            .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    }

    fn content_length(&self) -> Option<usize> {
        self.header("content-length")?.parse().ok()
    }

    fn is_sdp(&self, body: &[u8]) -> bool {
        self.header("content-type")
            .is_some_and(|value| value.to_ascii_lowercase().contains("sdp"))
            || body.starts_with(b"v=0")
    }

    /// The head with `Content-Length` set to `length`.
    fn with_length(&self, length: usize) -> Vec<u8> {
        let text = String::from_utf8_lossy(&self.raw);
        let mut out = String::with_capacity(text.len() + 8);
        for line in text.split_inclusive("\r\n") {
            let is_length = line
                .split_once(':')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"));
            if is_length {
                out.push_str(&format!("Content-Length: {length}\r\n"));
            } else {
                out.push_str(line);
            }
        }
        out.into_bytes()
    }
}

/// What to do with the response to a request, which arrive in the same order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Keep,
    Answer,
    /// Framing could not be followed: everything from here on passes through untouched.
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Http,
    Raw,
}

/// Buffered reading for message framing.
struct Reader<R> {
    inner: R,
    buffer: BytesMut,
    eof: bool,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: BytesMut::with_capacity(READ_CHUNK),
            eof: false,
        }
    }

    /// Reads more. `false` once the stream has ended.
    async fn fill(&mut self) -> io::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        self.buffer.reserve(READ_CHUNK);
        if self.inner.read_buf(&mut self.buffer).await? == 0 {
            self.eof = true;
            return Ok(false);
        }
        Ok(true)
    }

    /// The next head, `None` at a clean end, or `Err(InvalidData)` when there is none to find.
    async fn head(&mut self) -> io::Result<Option<Head>> {
        loop {
            if let Some(end) = find(&self.buffer, b"\r\n\r\n") {
                let raw = self.buffer.split_to(end + 4).to_vec();
                return Head::parse(raw)
                    .map(Some)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData));
            }
            if self.buffer.len() > MAX_HEAD {
                return Err(io::ErrorKind::InvalidData.into());
            }
            if !self.fill().await? {
                return if self.buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
        }
    }

    async fn exact(&mut self, length: usize) -> io::Result<Vec<u8>> {
        while self.buffer.len() < length {
            if !self.fill().await? {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        Ok(self.buffer.split_to(length).to_vec())
    }

    async fn until_close(&mut self) -> io::Result<Vec<u8>> {
        while self.fill().await? {
            if self.buffer.len() > MAX_BODY {
                return Err(io::ErrorKind::InvalidData.into());
            }
        }
        Ok(self.buffer.split().to_vec())
    }

    /// Hands what is buffered, then everything after it, to `out` untouched.
    async fn pass_rest(&mut self, out: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        out.write_all(&self.buffer).await?;
        self.buffer.advance(self.buffer.len());
        if !self.eof {
            tokio::io::copy(&mut self.inner, out).await?;
        }
        Ok(())
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn looks_like_http_request(start: &[u8]) -> bool {
    METHODS
        .iter()
        .any(|method| start.starts_with(method.as_bytes()) || method.as_bytes().starts_with(start))
}

/// Shared between the two directions of one connection.
struct Shared {
    expect: Mutex<VecDeque<Expect>>,
    mode: watch::Sender<Option<Mode>>,
}

impl Shared {
    fn push(&self, expect: Expect) {
        self.expect
            .lock()
            .expect("signaling lock")
            .push_back(expect);
    }

    fn pop(&self) -> Expect {
        self.expect
            .lock()
            .expect("signaling lock")
            .pop_front()
            .unwrap_or(Expect::Keep)
    }
}

/// Copies one game connection to and from `tunnel`, rewriting NetherNet join signaling on the
/// way. Ends when both directions have.
pub async fn rewrite_signaling<G, T>(game: G, tunnel: T, relay: Arc<Relay>)
where
    G: AsyncRead + AsyncWrite + Unpin,
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (game_in, mut game_out) = tokio::io::split(game);
    let (tunnel_in, mut tunnel_out) = tokio::io::split(tunnel);
    let (mode, mode_rx) = watch::channel(None);
    let shared = Shared {
        expect: Mutex::default(),
        mode,
    };

    let requests = async {
        let result = requests(Reader::new(game_in), &mut tunnel_out, &shared, &relay).await;
        // An undecided mode would leave the response side waiting forever.
        shared.mode.send_if_modified(|mode| {
            let undecided = mode.is_none();
            if undecided {
                *mode = Some(Mode::Raw);
            }
            undecided
        });
        let _ = tunnel_out.shutdown().await;
        result
    };
    let responses = async {
        let result = responses(
            Reader::new(tunnel_in),
            &mut game_out,
            &shared,
            mode_rx,
            &relay,
        )
        .await;
        let _ = game_out.shutdown().await;
        result
    };
    let (requests, responses) = tokio::join!(requests, responses);
    if let Err(error) = requests.and(responses) {
        debug!(%error, "signaling connection ended");
    }
}

async fn requests(
    mut game: Reader<impl AsyncRead + Unpin>,
    tunnel: &mut (impl AsyncWrite + Unpin),
    shared: &Shared,
    relay: &Relay,
) -> io::Result<()> {
    // Enough to tell a request line from a TLS handshake.
    while game.buffer.len() < 8 && game.fill().await? {}
    if !looks_like_http_request(&game.buffer[..game.buffer.len().min(8)]) {
        shared.mode.send_replace(Some(Mode::Raw));
        return game.pass_rest(tunnel).await;
    }
    shared.mode.send_replace(Some(Mode::Http));

    loop {
        let head = match game.head().await {
            Ok(Some(head)) => head,
            Ok(None) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                warn!("could not follow a signaling request, passing the rest through");
                shared.push(Expect::Raw);
                return game.pass_rest(tunnel).await;
            }
            Err(error) => return Err(error),
        };
        let length = head.content_length().unwrap_or(0);
        if head.chunked() || length > MAX_BODY {
            warn!("unexpected signaling request framing, passing the rest through");
            shared.push(Expect::Raw);
            tunnel.write_all(&head.raw).await?;
            return game.pass_rest(tunnel).await;
        }
        let body = game.exact(length).await?;

        let mut parts = head.first_line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let offer = method == "POST" && path.starts_with("/v1/join/") && head.is_sdp(&body);
        let rewritten = offer
            .then(|| std::str::from_utf8(&body).ok())
            .flatten()
            .map(rewrite_offer);
        match rewritten {
            Some((sdp, game_addresses)) => {
                debug!(
                    addresses = game_addresses.len(),
                    "rewrote a NetherNet offer"
                );
                relay.allow(game_addresses);
                shared.push(Expect::Answer);
                tunnel.write_all(&head.with_length(sdp.len())).await?;
                tunnel.write_all(sdp.as_bytes()).await?;
            }
            None => {
                shared.push(Expect::Keep);
                tunnel.write_all(&head.raw).await?;
                tunnel.write_all(&body).await?;
            }
        }
    }
}

async fn responses(
    mut tunnel: Reader<impl AsyncRead + Unpin>,
    game: &mut (impl AsyncWrite + Unpin),
    shared: &Shared,
    mut mode: watch::Receiver<Option<Mode>>,
    relay: &Relay,
) -> io::Result<()> {
    let decided = *mode
        .wait_for(Option::is_some)
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
    if decided == Some(Mode::Raw) {
        return tunnel.pass_rest(game).await;
    }

    loop {
        let head = match tunnel.head().await {
            Ok(Some(head)) => head,
            Ok(None) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return tunnel.pass_rest(game).await;
            }
            Err(error) => return Err(error),
        };
        let status: u16 = head
            .first_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        if (100..200).contains(&status) {
            // Informational: the real response to the same request follows.
            game.write_all(&head.raw).await?;
            continue;
        }
        let expect = shared.pop();
        if expect == Expect::Raw || head.chunked() {
            game.write_all(&head.raw).await?;
            return tunnel.pass_rest(game).await;
        }
        let body = match head.content_length() {
            _ if status == 204 || status == 304 => Vec::new(),
            Some(length) if length <= MAX_BODY => tunnel.exact(length).await?,
            Some(_) => {
                game.write_all(&head.raw).await?;
                return tunnel.pass_rest(game).await;
            }
            None => tunnel.until_close().await?,
        };

        let answer = expect == Expect::Answer && (200..300).contains(&status) && head.is_sdp(&body);
        let rewritten = answer
            .then(|| std::str::from_utf8(&body).ok())
            .flatten()
            .map(|sdp| rewrite_answer(sdp, relay.candidate));
        match rewritten {
            Some((sdp, server)) => {
                info!(relay = %relay.candidate, "the game's NetherNet connection rides the tunnel");
                debug!(?server, "the server's own NetherNet addresses");
                game.write_all(&head.with_length(sdp.len())).await?;
                game.write_all(sdp.as_bytes()).await?;
            }
            None => {
                game.write_all(&head.raw).await?;
                game.write_all(&body).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests;
