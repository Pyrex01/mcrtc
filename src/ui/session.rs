//! Runs the host and join flows for the window, the same ones the terminal drives. Lines go in
//! through a [`Console`] and [`Event`]s come out on a channel the window drains every frame.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use crate::config::{IceConfig, TurnServer};
use crate::console::Console;
use crate::edition::Edition;
use crate::events::{Event, Reporter};
use crate::host::{self, HostOptions};
use crate::join::{self, JoinOptions};
use crate::peer::PeerOptions;
use crate::sync::SyncOptions;

const SESSION_STOP_TIMEOUT: Duration = Duration::from_secs(8);

pub struct UiOptions {
    pub peer: PeerOptions,
    /// Selected at first. `server` and `listen` belong to this edition.
    pub edition: Edition,
    pub server: String,
    pub listen: String,
    pub max_peers: Option<usize>,
    pub max_connections: Option<usize>,
    pub whitelist: Option<HashSet<String>>,
    /// From the config file. The window's folder fields override the folders.
    pub sync: Option<SyncOptions>,
    pub sync_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Host,
    Join,
}

/// What the forms hold. Empty strings mean "use the default".
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub edition: Edition,
    pub server: String,
    pub listen: String,
    pub max_peers: Option<usize>,
    pub max_connections: Option<usize>,
    pub stun: String,
    pub turn_url: String,
    pub turn_user: String,
    pub turn_pass: String,
    pub world: String,
    pub sync_path: String,
}

struct Session {
    input: mpsc::Sender<String>,
    stop: watch::Sender<bool>,
}

/// Called after every event, so an idle window wakes up to show it.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct Sessions {
    options: UiOptions,
    runtime: Handle,
    feed: std_mpsc::Sender<Event>,
    wake: Wake,
    session: Arc<Mutex<Option<Session>>>,
}

impl Sessions {
    pub fn new(
        options: UiOptions,
        runtime: Handle,
        wake: Wake,
    ) -> (Self, std_mpsc::Receiver<Event>) {
        let (feed, events) = std_mpsc::channel();
        let sessions = Self {
            options,
            runtime,
            feed,
            wake,
            session: Arc::default(),
        };
        (sessions, events)
    }

    pub fn first_edition(&self) -> Edition {
        self.options.edition
    }

    pub fn default_server(&self, edition: Edition) -> &str {
        if edition == self.options.edition {
            &self.options.server
        } else {
            edition.default_address()
        }
    }

    pub fn default_listen(&self, edition: Edition) -> &str {
        if edition == self.options.edition {
            &self.options.listen
        } else {
            edition.default_address()
        }
    }

    pub fn default_world(&self) -> Option<&std::path::Path> {
        self.options
            .sync
            .as_ref()
            .map(|sync| sync.world_path.as_path())
    }

    pub fn default_sync_path(&self) -> Option<&std::path::Path> {
        self.options.sync_path.as_deref()
    }

    fn resolve_ice(&self, settings: &Settings) -> Result<IceConfig> {
        let mut ice = self.options.peer.ice.clone();
        let stun: Vec<_> = settings
            .stun
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !stun.is_empty() {
            ice.stun = stun;
        }
        if !settings.turn_url.trim().is_empty() {
            ice.turn.push(TurnServer {
                url: settings.turn_url.trim().to_owned(),
                username: settings.turn_user.clone(),
                password: settings.turn_pass.clone(),
            });
        }
        ice.validate()?;
        Ok(ice)
    }

    pub fn start(&self, mode: Mode, settings: Settings) -> Result<()> {
        let mut session = self.session.lock().expect("session lock");
        if session.is_some() {
            bail!("a session is already running");
        }

        let mut peer = self.options.peer.clone();
        peer.ice = self.resolve_ice(&settings)?;

        let (input, lines) = mpsc::channel(16);
        let (stop, stopped) = watch::channel(false);
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let reporter = Reporter::channel(events_tx);
        let console = Console::from_channel(lines);

        let (feed, wake) = (self.feed.clone(), self.wake.clone());
        self.runtime.spawn(async move {
            while let Some(event) = events.recv().await {
                let ended = matches!(event, Event::SessionEnded { .. });
                let _ = feed.send(event);
                wake();
                if ended {
                    return;
                }
            }
        });

        let or_default = |value: String, default: &str| match value.trim() {
            "" => default.to_owned(),
            value => value.to_owned(),
        };
        let edition = settings.edition;
        let server = or_default(settings.server, self.default_server(edition));
        let listen = or_default(settings.listen, self.default_listen(edition));
        let max_peers = settings.max_peers.or(self.options.max_peers);
        let max_connections = settings.max_connections.or(self.options.max_connections);
        let whitelist = self.options.whitelist.clone();
        let sync = match settings.world.trim() {
            "" => self.options.sync.clone(),
            world => Some(SyncOptions {
                world_path: world.into(),
                ..self.options.sync.clone().unwrap_or(SyncOptions {
                    world_path: PathBuf::new(),
                    interval_secs: crate::sync::DEFAULT_INTERVAL_SECS,
                    rcon_address: None,
                    rcon_password: None,
                })
            }),
        };
        let sync_path = match settings.sync_path.trim() {
            "" => self.options.sync_path.clone(),
            path => Some(path.into()),
        };
        let running = self.session.clone();
        self.runtime.spawn(async move {
            let result = match mode {
                Mode::Host => {
                    let options = HostOptions {
                        peer,
                        edition,
                        server,
                        max_peers,
                        max_connections,
                        whitelist,
                        raw_json: false,
                        reporter: reporter.clone(),
                        sync,
                    };
                    host::run(options, console, stopped).await
                }
                Mode::Join => {
                    let options = JoinOptions {
                        peer,
                        edition,
                        listen,
                        raw_json: false,
                        reporter: reporter.clone(),
                        sync_path,
                    };
                    join::run(options, console, stopped).await
                }
            };
            // Cleared before reporting, so the window can start again as soon as it hears this.
            *running.lock().expect("session lock") = None;
            let error = result.err().map(|error| format!("{error:#}"));
            reporter.report(Event::SessionEnded { error });
        });

        *session = Some(Session { input, stop });
        Ok(())
    }

    /// Never waits, because the window calls this between frames.
    pub fn send(&self, line: String) -> Result<()> {
        let session = self.session.lock().expect("session lock");
        let session = session
            .as_ref()
            .ok_or_else(|| anyhow!("no session is running"))?;
        session
            .input
            .try_send(line)
            .map_err(|_| anyhow!("mcrtc is not listening right now"))
    }

    pub fn stop(&self) {
        if let Some(session) = self.session.lock().expect("session lock").as_ref() {
            session.stop.send_replace(true);
        }
    }

    fn running(&self) -> bool {
        self.session.lock().expect("session lock").is_some()
    }

    /// Stops the session and gives it time to say goodbye. For after the window has closed.
    pub fn shut_down(&self) {
        self.stop();
        let stopped = async {
            while self.running() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = self
            .runtime
            .block_on(async { timeout(SESSION_STOP_TIMEOUT, stopped).await });
    }
}
