//! A real host and a [`MobileJoin`] in one process on loopback, carrying Bedrock UDP.

use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::mpsc;
use std::time::Duration;

use mcrtc::console::Console;
use mcrtc::events::{Event, Reporter};
use mcrtc::host::{self, HostOptions};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc as tokio_mpsc, watch};

use super::*;

const LIMIT: Duration = Duration::from_secs(60);

struct Forward(mpsc::Sender<MobileEvent>);

impl EventListener for Forward {
    fn on_event(&self, event: MobileEvent) {
        let _ = self.0.send(event);
    }
}

fn free_port() -> SocketAddr {
    StdUdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

#[test]
fn joins_a_host_and_carries_bedrock_datagrams() {
    init_logging(false);
    let runtime = Runtime::new().unwrap();

    // The "Bedrock server": echoes every datagram back.
    let server = runtime.block_on(async {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buffer = [0; 1500];
            while let Ok((len, from)) = socket.recv_from(&mut buffer).await {
                let _ = socket.send_to(&buffer[..len], from).await;
            }
        });
        address
    });

    let (host_events_tx, mut host_events) = tokio_mpsc::unbounded_channel();
    let (host_lines, host_console) = tokio_mpsc::channel(16);
    let (stop_host, host_stopped) = watch::channel(false);
    let host = runtime.spawn(host::run(
        HostOptions {
            peer: PeerOptions::loopback(),
            edition: Edition::Bedrock,
            server: server.to_string(),
            max_peers: None,
            max_connections: None,
            whitelist: None,
            raw_json: false,
            reporter: Reporter::channel(host_events_tx),
        },
        Console::from_channel(host_console),
        host_stopped,
    ));

    let (events_tx, events) = mpsc::channel();
    let joiner = MobileJoin::with_peer(
        PeerOptions::loopback(),
        "test-id".to_owned(),
        Box::new(Forward(events_tx)),
    )
    .unwrap();
    assert_eq!(joiner.my_id(), "test-id");
    let listen = free_port();
    joiner
        .start(MobileSettings {
            listen: listen.to_string(),
            udp_only: true,
            ..MobileSettings::default()
        })
        .unwrap();
    // Only one session at a time.
    assert!(joiner.start(MobileSettings::default()).is_err());

    // Host: name the friend, then hand over the invite.
    let invite = runtime.block_on(async {
        loop {
            match tokio::time::timeout(LIMIT, host_events.recv())
                .await
                .unwrap()
            {
                Some(Event::AskName { .. }) => host_lines.send(String::new()).await.unwrap(),
                Some(Event::Invite { text, .. }) => return text,
                Some(_) => {}
                None => panic!("the host ended early"),
            }
        }
    });

    // Join: a bad paste first, then the invite, then carry the reply back.
    let mut seen_bad_paste = false;
    let address = loop {
        match events.recv_timeout(LIMIT).unwrap() {
            MobileEvent::AskInvite if !seen_bad_paste => joiner.send("garbage".to_owned()).unwrap(),
            MobileEvent::BadPaste { .. } => seen_bad_paste = true,
            MobileEvent::AskInvite => joiner.send(invite.clone()).unwrap(),
            MobileEvent::Reply { text } => runtime.block_on(host_lines.send(text)).unwrap(),
            MobileEvent::Joined { address, .. } => break address,
            MobileEvent::SessionEnded { error } => panic!("join ended early: {error:?}"),
            MobileEvent::Other { description } => panic!("unexpected event {description}"),
            _ => {}
        }
    };
    assert!(seen_bad_paste);
    assert_eq!(address, listen.to_string());
    // UDP only: the game's TCP attempt is refused, so it falls back to RakNet.
    assert!(std::net::TcpStream::connect(listen).is_err());

    // Minecraft's side: a datagram to the tunnel comes back from the server.
    runtime.block_on(async {
        let game = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buffer = [0; 1500];
        let reply = tokio::time::timeout(LIMIT, async {
            // The first datagrams can race the data channel opening, so keep trying.
            loop {
                game.send_to(b"hello bedrock", listen).await.unwrap();
                if let Ok(Ok((len, _))) =
                    tokio::time::timeout(Duration::from_millis(500), game.recv_from(&mut buffer))
                        .await
                {
                    return buffer[..len].to_vec();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(reply, b"hello bedrock");
    });

    joiner.stop();
    loop {
        if let MobileEvent::SessionEnded { error } = events.recv_timeout(LIMIT).unwrap() {
            assert_eq!(error, None);
            break;
        }
    }
    joiner.shut_down();
    stop_host.send_replace(true);
    runtime.block_on(async {
        tokio::time::timeout(LIMIT, host)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    });
    drop(joiner);
}

#[test]
fn host_only_events_are_passed_on_as_other() {
    let event = MobileEvent::from(Event::Hosting {
        server: "127.0.0.1:19132".into(),
    });
    assert!(matches!(event, MobileEvent::Other { .. }));
    assert_eq!(MobileEvent::from(Event::AskInvite), MobileEvent::AskInvite);
}

#[test]
fn the_id_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("mcrtc-android-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_text = dir.to_string_lossy().into_owned();
    let (tx, _rx) = mpsc::channel();
    let first = MobileJoin::new(dir_text.clone(), Box::new(Forward(tx.clone()))).unwrap();
    let second = MobileJoin::new(dir_text, Box::new(Forward(tx))).unwrap();
    assert_eq!(first.my_id(), second.my_id());
    drop((first, second));
    let _ = std::fs::remove_dir_all(dir);
}
