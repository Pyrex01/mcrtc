//! The whole tunnel in one process on loopback: two real peer connections with host candidates
//! only, real TCP on both ends, and no external network.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout, Instant};

use crate::bridge::{Counter, BUFFERED_HIGH};
use crate::control::{self, PendingOpens, Role};
use crate::peer::test_support::*;
use crate::peer::{Peer, PeerOptions};
use crate::{host, join};

const LIMIT: Duration = Duration::from_secs(60);

fn random_bytes(len: usize) -> Vec<u8> {
    let mut data = vec![0; len];
    rand::rng().fill(&mut data[..]);
    data
}

/// Runs `handle` for each connection and counts the connections still open.
async fn fake_server<F, Fut>(handle: F) -> (SocketAddr, Arc<AtomicUsize>)
where
    F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let open = Arc::new(AtomicUsize::new(0));
    let counted = open.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            counted.fetch_add(1, Ordering::SeqCst);
            let counted = counted.clone();
            let served = handle(stream);
            tokio::spawn(async move {
                served.await;
                counted.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (address, open)
}

async fn echo(mut stream: TcpStream) {
    let (mut reader, mut writer) = stream.split();
    let _ = tokio::io::copy(&mut reader, &mut writer).await;
}

struct Harness {
    listen: SocketAddr,
    host: Peer,
    joiner: Peer,
    host_connections: Counter,
    join_connections: Counter,
    _stop: watch::Sender<bool>,
}

impl Harness {
    async fn start(server: SocketAddr, max_connections: Option<usize>) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let host = Peer::new(PeerOptions::loopback(), ignore_channels())
            .await
            .unwrap();
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let joiner = Peer::new(
            PeerOptions::loopback(),
            Arc::new(move |channel| {
                let _ = control_tx.send(channel);
            }),
        )
        .await
        .unwrap();
        let host_control = host.create_channel(control::LABEL).await.unwrap();
        connect_in_memory(&host, &joiner).await.unwrap();

        // Like the real join flow: the connection state turns connected before SCTP is up, and
        // only the arrival of the control channel proves new channels can be opened.
        let join_control = timeout(LIMIT, control_rx.recv()).await.unwrap().unwrap();

        let (stop, stopped) = watch::channel(false);
        let host_connections = Counter::default();
        let host_tunnel = host::Tunnel {
            peer: host.clone(),
            control: host_control.clone(),
            server: server.to_string(),
            max_connections,
            label: "test".into(),
            connections: host_connections.clone(),
        };
        tokio::spawn(control::run(
            host_control,
            host_tunnel.role(),
            stopped.clone(),
            watch::channel(None).0,
        ));

        let join_connections = Counter::default();
        let pending = PendingOpens::default();
        let join_tunnel = join::Tunnel {
            peer: joiner.clone(),
            control: join_control.clone(),
            pending: pending.clone(),
            connections: join_connections.clone(),
        };
        tokio::spawn(control::run(
            join_control,
            Role::Joiner(pending),
            stopped.clone(),
            watch::channel(None).0,
        ));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen = listener.local_addr().unwrap();
        tokio::spawn(join_tunnel.serve(listener, stopped));
        Self {
            listen,
            host,
            joiner,
            host_connections,
            join_connections,
            _stop: stop,
        }
    }

    async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.listen).await.unwrap()
    }

    async fn assert_idle(&self) {
        wait_until("both sides to release their connections", || {
            self.host_connections.get() == 0 && self.join_connections.get() == 0
        })
        .await;
    }

    async fn close(self) {
        self.host.close().await;
        self.joiner.close().await;
    }
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(10)).await;
    }
}

async fn assert_echoes(mut stream: TcpStream, data: Vec<u8>) {
    let (mut reader, mut writer) = stream.split();
    let mut received = vec![0; data.len()];
    let (written, read) = tokio::join!(writer.write_all(&data), reader.read_exact(&mut received));
    written.unwrap();
    read.unwrap();
    assert!(received == data, "the echoed bytes differ");
}

#[tokio::test(flavor = "multi_thread")]
async fn several_megabytes_come_back_identical() {
    let (server, _) = fake_server(echo).await;
    let harness = Harness::start(server, None).await;
    let stream = harness.connect().await;
    timeout(LIMIT, assert_echoes(stream, random_bytes(8 * 1024 * 1024)))
        .await
        .unwrap();
    harness.assert_idle().await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_connections_do_not_mix() {
    let (server, open) = fake_server(echo).await;
    let harness = Harness::start(server, None).await;
    let mut clients = JoinSet::new();
    for _ in 0..8 {
        let stream = harness.connect().await;
        clients.spawn(assert_echoes(stream, random_bytes(2 * 1024 * 1024)));
    }
    timeout(LIMIT, async {
        while let Some(result) = clients.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    harness.assert_idle().await;
    wait_until("the server connections to close", || {
        open.load(Ordering::SeqCst) == 0
    })
    .await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_close_reaches_the_server() {
    let (server, open) = fake_server(echo).await;
    let harness = Harness::start(server, None).await;
    let mut stream = harness.connect().await;
    stream.write_all(b"ping").await.unwrap();
    let mut reply = [0; 4];
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(open.load(Ordering::SeqCst), 1);

    drop(stream);
    wait_until("the server to see EOF", || open.load(Ordering::SeqCst) == 0).await;
    harness.assert_idle().await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_close_delivers_everything_then_eof() {
    let data = Arc::new(random_bytes(5 * 1024 * 1024));
    let served = data.clone();
    let (server, _) = fake_server(move |mut stream| {
        let served = served.clone();
        async move {
            stream.write_all(&served).await.unwrap();
        }
    })
    .await;
    let harness = Harness::start(server, None).await;
    let mut stream = harness.connect().await;

    let mut received = Vec::new();
    timeout(LIMIT, stream.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.len(), data.len());
    assert!(received == *data, "the bytes differ");
    harness.assert_idle().await;
    harness.close().await;
}

/// A status ping: a few bytes each way, then both sides hang up.
#[tokio::test(flavor = "multi_thread")]
async fn many_short_connections_leave_nothing_behind() {
    let (server, open) = fake_server(echo).await;
    let harness = Harness::start(server, None).await;
    timeout(Duration::from_secs(120), async {
        for round in 0..400u32 {
            let mut stream = harness.connect().await;
            stream.write_all(&round.to_be_bytes()).await.unwrap();
            let mut reply = [0; 4];
            stream
                .read_exact(&mut reply)
                .await
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            assert_eq!(reply, round.to_be_bytes());
        }
    })
    .await
    .unwrap();
    harness.assert_idle().await;
    wait_until("the server connections to close", || {
        open.load(Ordering::SeqCst) == 0
    })
    .await;
    harness.close().await;
}

/// Several clients reconnecting in parallel, so closes and opens constantly overlap.
#[tokio::test(flavor = "multi_thread")]
async fn parallel_churn_keeps_working() {
    let (server, open) = fake_server(echo).await;
    let harness = Harness::start(server, None).await;
    let listen = harness.listen;
    let mut clients = JoinSet::new();
    for client in 0..8u32 {
        clients.spawn(async move {
            for round in 0..60u32 {
                let message = (client * 1000 + round).to_be_bytes();
                let mut stream = TcpStream::connect(listen).await.unwrap();
                stream.write_all(&message).await.unwrap();
                let mut reply = [0; 4];
                stream
                    .read_exact(&mut reply)
                    .await
                    .unwrap_or_else(|error| panic!("client {client} round {round}: {error}"));
                assert_eq!(reply, message);
            }
        });
    }
    timeout(Duration::from_secs(120), async {
        while let Some(result) = clients.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    harness.assert_idle().await;
    wait_until("the server connections to close", || {
        open.load(Ordering::SeqCst) == 0
    })
    .await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_closes_the_client() {
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = unused.local_addr().unwrap();
    drop(unused);

    let harness = Harness::start(server, None).await;
    let mut stream = harness.connect().await;
    let mut buffer = [0; 1];
    let read = timeout(Duration::from_secs(20), stream.read(&mut buffer))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    harness.assert_idle().await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_limit_refuses_extras_only() {
    let (server, _) = fake_server(echo).await;
    let harness = Harness::start(server, Some(1)).await;

    let mut first = harness.connect().await;
    first.write_all(b"one").await.unwrap();
    let mut reply = [0; 3];
    first.read_exact(&mut reply).await.unwrap();

    let mut second = harness.connect().await;
    let mut buffer = [0; 1];
    let read = timeout(Duration::from_secs(20), second.read(&mut buffer))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");

    first.write_all(b"two").await.unwrap();
    first.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"two");

    drop(first);
    harness.assert_idle().await;
    let third = harness.connect().await;
    timeout(LIMIT, assert_echoes(third, b"three".to_vec()))
        .await
        .unwrap();
    harness.close().await;
}

/// The server stops reading, so backpressure has to reach the client instead of the heap.
#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_server_stops_the_client() {
    let (resume, resumed) = watch::channel(false);
    let total = Arc::new(AtomicUsize::new(0));
    let counted = total.clone();
    let (server, _) = fake_server(move |mut stream| {
        let mut resumed = resumed.clone();
        let counted = counted.clone();
        async move {
            let _ = resumed.wait_for(|go| *go).await;
            let mut buffer = vec![0; 64 * 1024];
            while let Ok(read) = stream.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                counted.fetch_add(read, Ordering::SeqCst);
            }
        }
    })
    .await;
    let harness = Harness::start(server, None).await;
    let mut stream = harness.connect().await;

    // Far more than every buffer on the way can hold.
    let chunk = random_bytes(256 * 1024);
    let attempted = 256 * 1024 * 1024;
    let mut written = 0;
    let stalled = timeout(Duration::from_secs(5), async {
        while written < attempted {
            stream.write_all(&chunk).await.unwrap();
            written += chunk.len();
        }
    })
    .await;
    assert!(
        stalled.is_err(),
        "the client wrote {written} bytes without being held back"
    );
    eprintln!("client was held back after {written} bytes");
    assert!(written < 64 * 1024 * 1024, "{written} bytes were buffered");

    // Everything accepted so far still arrives once the server wakes up.
    resume.send_replace(true);
    wait_until("the stalled bytes to arrive", || {
        total.load(Ordering::SeqCst) >= written
    })
    .await;
    drop(stream);
    harness.assert_idle().await;
    harness.close().await;
}

/// A bidirectional flood through a server that writes before it reads. If pausing the sender
/// depended on the same event loop the receiver stalls, this would deadlock.
#[tokio::test(flavor = "multi_thread")]
async fn both_directions_flooded_at_once() {
    let size = 12 * BUFFERED_HIGH;
    let (server, _) = fake_server(move |mut stream| async move {
        let data = vec![7; size];
        let (mut reader, mut writer) = stream.split();
        let mut sink = vec![0; size];
        let (written, read) = tokio::join!(writer.write_all(&data), reader.read_exact(&mut sink));
        written.unwrap();
        read.unwrap();
    })
    .await;
    let harness = Harness::start(server, None).await;
    let mut stream = harness.connect().await;
    let (mut reader, mut writer) = stream.split();
    let data = vec![9; size];
    let mut sink = vec![0; size];
    let (written, read) = timeout(LIMIT, async {
        tokio::join!(writer.write_all(&data), reader.read_exact(&mut sink))
    })
    .await
    .unwrap();
    written.unwrap();
    read.unwrap();
    assert!(sink.iter().all(|byte| *byte == 7));
    drop(stream);
    harness.assert_idle().await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_the_connection_ends_every_bridge() {
    let (server, open) = fake_server(echo).await;
    let harness = Harness::start(server, None).await;
    let mut clients = Vec::new();
    for _ in 0..4 {
        let mut stream = harness.connect().await;
        stream.write_all(b"hi").await.unwrap();
        let mut reply = [0; 2];
        stream.read_exact(&mut reply).await.unwrap();
        clients.push(stream);
    }
    assert_eq!(harness.host_connections.get(), 4);

    harness.host.close().await;
    harness.joiner.close().await;
    harness.assert_idle().await;
    wait_until("the server connections to close", || {
        open.load(Ordering::SeqCst) == 0
    })
    .await;
    for mut stream in clients {
        let mut buffer = [0; 1];
        let read = timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .unwrap();
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    }
}

/// Humans are slow: the reply can take far longer than ICE normally waits to reach the host.
#[tokio::test(flavor = "multi_thread")]
async fn the_host_can_paste_the_reply_late() {
    let host = Peer::new(PeerOptions::loopback(), ignore_channels())
        .await
        .unwrap();
    let joiner = Peer::new(PeerOptions::loopback(), ignore_channels())
        .await
        .unwrap();
    let _control = host.create_channel(control::LABEL).await.unwrap();
    let offer = host.create_offer().await.unwrap();
    let answer = joiner.accept_offer(offer).await.unwrap();

    // Past the library's 30 second default.
    sleep(Duration::from_secs(36)).await;

    host.accept_answer(answer).await.unwrap();
    host.wait_connected(Duration::from_secs(10)).await.unwrap();
    joiner
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    host.close().await;
    joiner.close().await;
}
