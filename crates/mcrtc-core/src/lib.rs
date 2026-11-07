//! Platform-neutral mcrtc networking and session protocol.

#[path = "../../../src/bridge.rs"]
pub mod bridge;
#[path = "../../../src/config.rs"]
pub mod config;
pub mod console;
#[path = "../../../src/control.rs"]
pub mod control;
#[path = "../../../src/datagram.rs"]
pub mod datagram;
#[path = "../../../src/edition.rs"]
pub mod edition;
#[path = "../../../src/events.rs"]
pub mod events;
#[path = "../../../src/host.rs"]
pub mod host;
#[path = "../../../src/invite.rs"]
pub mod invite;
#[path = "../../../src/join.rs"]
pub mod join;
#[path = "../../../src/peer.rs"]
pub mod peer;

pub use config::TurnServer;
pub use edition::Edition;
pub use events::Event;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Host,
    Join,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub server: String,
    pub listen: String,
    pub stun: Vec<String>,
    pub turn: Vec<TurnServer>,
    pub identity_path: Option<PathBuf>,
    pub whitelist: Option<HashSet<String>>,
    pub max_peers: Option<usize>,
    pub max_connections: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Start { mode: Mode, config: Config },
    SubmitName(String),
    SubmitInvite(String),
    SubmitReply(String),
    Stop,
}

/// A binding-friendly session handle. The runtime and all networking tasks remain on the Rust
/// side; callers only send commands and receive frontend-neutral events.
pub struct Session {
    input: mpsc::Sender<String>,
    stop: watch::Sender<bool>,
    events: std_mpsc::Receiver<Event>,
}

impl Session {
    pub fn start(mode: Mode, config: Config) -> anyhow::Result<Self> {
        let (input, lines) = mpsc::channel(32);
        let (stop, stopped) = watch::channel(false);
        let (events_tx, events) = std_mpsc::channel();
        let mut peer = peer::PeerOptions::new(config::IceConfig {
            stun: config.stun,
            turn: config.turn,
        });
        if let Some(path) = config.identity_path.as_deref() {
            peer.certificate = Some(peer::load_or_create_identity(path)?);
        }
        let reporter = events::Reporter::channel({
            let (tx, mut rx) = mpsc::unbounded_channel();
            let output = events_tx.clone();
            std::thread::spawn(move || {
                while let Some(event) = rx.blocking_recv() {
                    let _ = output.send(event);
                }
            });
            tx
        });
        let console = console::Console::from_channel(lines);
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Runtime::new() else {
                return;
            };
            runtime.block_on(async move {
                let _ = events_tx.send(Event::Started {
                    mode: format!("{mode:?}"),
                });
                let result = match mode {
                    Mode::Host => {
                        host::run(
                            host::HostOptions {
                                peer,
                                edition: Edition::Java,
                                server: config.server,
                                max_peers: config.max_peers,
                                max_connections: config.max_connections,
                                whitelist: config.whitelist,
                                raw_json: false,
                                reporter,
                            },
                            console,
                            stopped,
                        )
                        .await
                    }
                    Mode::Join => {
                        join::run(
                            join::JoinOptions {
                                peer,
                                edition: Edition::Java,
                                listen: config.listen,
                                raw_json: false,
                                reporter,
                            },
                            console,
                            stopped,
                        )
                        .await
                    }
                };
                if let Err(error) = result {
                    let _ = events_tx.send(Event::Failed {
                        message: format!("{error:#}"),
                    });
                }
                let _ = events_tx.send(Event::Stopped);
            });
        });
        Ok(Self {
            input,
            stop,
            events,
        })
    }

    pub fn send(&self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::SubmitName(value)
            | Command::SubmitInvite(value)
            | Command::SubmitReply(value) => self
                .input
                .try_send(value)
                .map_err(|e| anyhow::anyhow!(e.to_string())),
            Command::Stop => {
                self.stop.send_replace(true);
                Ok(())
            }
            Command::Start { .. } => Err(anyhow::anyhow!("a session is already started")),
        }
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
    pub fn recv(&self) -> Option<Event> {
        self.events.recv().ok()
    }
}
