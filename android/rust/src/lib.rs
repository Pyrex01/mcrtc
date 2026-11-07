//! The mcrtc tunnel core for the Android app: join a Minecraft world hosted with mcrtc on a PC.
//!
//! Kotlin drives this through UniFFI bindings. It is a thin layer over [`mcrtc::session::Sessions`],
//! the same driver the desktop window uses: lines go in with [`MobileJoin::send`] and events come
//! back through an [`EventListener`].

use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc, Once};
use std::thread;

use mcrtc::config::{IceConfig, DEFAULT_STUN};
use mcrtc::edition::Edition;
use mcrtc::events::Event;
use mcrtc::peer::{certificate_id, load_or_create_identity, PeerOptions};
use mcrtc::session::{Mode, Sessions, Settings, UiOptions};
use tokio::runtime::Runtime;

uniffi::setup_scaffolding!();

const IDENTITY_FILE: &str = "identity.pem";

#[derive(Debug, uniffi::Error)]
pub enum MobileError {
    Failed { reason: String },
}

impl fmt::Display for MobileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { reason } => f.write_str(reason),
        }
    }
}

impl std::error::Error for MobileError {}

impl From<anyhow::Error> for MobileError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed {
            reason: format!("{error:#}"),
        }
    }
}

impl From<std::io::Error> for MobileError {
    fn from(error: std::io::Error) -> Self {
        Self::Failed {
            reason: error.to_string(),
        }
    }
}

/// The settings screen. Empty strings mean "use the default".
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct MobileSettings {
    /// STUN URLs separated by whitespace, replacing the defaults.
    pub stun: String,
    pub turn_url: String,
    pub turn_user: String,
    pub turn_pass: String,
    /// Where Minecraft connects. Defaults to 127.0.0.1:19132; 0.0.0.0:19132 also shows the world
    /// under LAN Games.
    pub listen: String,
    /// Listen on UDP only, for RakNet servers that also accept TCP on the Bedrock port, where a
    /// TCP connection would lead the game away from RakNet. Leave off for NetherNet servers
    /// (Geyser's default transport), which sign players in over TCP.
    pub udp_only: bool,
}

/// What the join flow has to say. Mirrors the join half of [`mcrtc::events::Event`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MobileEvent {
    /// The ID a host can put on its whitelist.
    YourId {
        id: String,
    },
    /// Waiting for an invite; answer with [`MobileJoin::send`].
    AskInvite,
    /// The pasted invite was rejected. Another [`MobileEvent::AskInvite`] follows.
    BadPaste {
        error: String,
    },
    Gathering,
    /// Send this to the host.
    Reply {
        text: String,
    },
    WaitingForHost,
    /// Minecraft can connect to `address` now.
    Joined {
        address: String,
        route: Option<String>,
    },
    Left {
        reason: String,
    },
    ConnectFailed {
        error: String,
    },
    InviteExpired,
    Cancelled,
    ShuttingDown,
    /// The flow is over; [`MobileJoin::start`] may be called again.
    SessionEnded {
        error: Option<String>,
    },
    /// Anything else, which a join session does not normally produce.
    Other {
        description: String,
    },
}

impl From<Event> for MobileEvent {
    fn from(event: Event) -> Self {
        match event {
            Event::YourId { id } => Self::YourId { id },
            Event::AskInvite => Self::AskInvite,
            Event::BadPaste { error } => Self::BadPaste { error },
            Event::Gathering => Self::Gathering,
            Event::Reply { text } => Self::Reply { text },
            Event::WaitingForHost => Self::WaitingForHost,
            Event::Joined { address, route } => Self::Joined { address, route },
            Event::Left { reason } => Self::Left { reason },
            Event::ConnectFailed { error } => Self::ConnectFailed { error },
            Event::InviteExpired => Self::InviteExpired,
            Event::Cancelled => Self::Cancelled,
            Event::ShuttingDown => Self::ShuttingDown,
            Event::SessionEnded { error } => Self::SessionEnded { error },
            other => Self::Other {
                description: format!("{other:?}"),
            },
        }
    }
}

/// Receives every event, on a background thread of its own.
#[uniffi::export(callback_interface)]
pub trait EventListener: Send + Sync {
    fn on_event(&self, event: MobileEvent);
}

/// Sends Rust logs to logcat under the tag `mcrtc`. Safe to call more than once.
#[uniffi::export]
pub fn init_logging(verbose: bool) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let filter = if verbose {
            "warn,mcrtc=debug,mcrtc_android=debug"
        } else {
            "mcrtc=info,mcrtc_android=info"
        };
        install_logging(tracing_subscriber::EnvFilter::new(filter));
    });
}

#[cfg(target_os = "android")]
fn install_logging(filter: tracing_subscriber::EnvFilter) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let _ = tracing_subscriber::registry()
        .with(filter)
        // Logcat shows escape codes as text.
        .with(paranoid_android::layer("mcrtc").with_ansi(false))
        .try_init();
}

#[cfg(not(target_os = "android"))]
fn install_logging(filter: tracing_subscriber::EnvFilter) {
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

/// One joiner: a Tokio runtime and at most one join session at a time.
///
/// Keep a single instance for the app's lifetime, owned by the foreground service.
#[derive(uniffi::Object)]
pub struct MobileJoin {
    sessions: Sessions,
    id: String,
    // Last, so the sessions are gone before the runtime they run on.
    _runtime: Runtime,
}

#[uniffi::export]
impl MobileJoin {
    /// `files_dir` is the app's private storage (`Context.filesDir`), which keeps the mcrtc ID
    /// stable across launches.
    #[uniffi::constructor]
    pub fn new(
        files_dir: String,
        listener: Box<dyn EventListener>,
    ) -> Result<Arc<Self>, MobileError> {
        let mut peer = PeerOptions::new(default_ice());
        let certificate = load_or_create_identity(&Path::new(&files_dir).join(IDENTITY_FILE))?;
        let id = certificate_id(&certificate)?;
        peer.certificate = Some(certificate);
        Ok(Arc::new(Self::with_peer(peer, id, listener)?))
    }

    /// The ID a host can put on its whitelist.
    pub fn my_id(&self) -> String {
        self.id.clone()
    }

    /// Starts a join session. Fails if one is already running or the settings are invalid.
    pub fn start(&self, settings: MobileSettings) -> Result<(), MobileError> {
        let settings = Settings {
            edition: Edition::Bedrock,
            listen: settings.listen,
            stun: settings.stun,
            turn_url: settings.turn_url,
            turn_user: settings.turn_user,
            turn_pass: settings.turn_pass,
            udp_only: settings.udp_only,
            ..Settings::default()
        };
        self.sessions.start(Mode::Join, settings)?;
        Ok(())
    }

    /// Answers the session: the pasted invite after [`MobileEvent::AskInvite`], or `cancel`.
    /// Never blocks.
    pub fn send(&self, line: String) -> Result<(), MobileError> {
        self.sessions.send(line)?;
        Ok(())
    }

    /// Asks the session to end. [`MobileEvent::SessionEnded`] follows. Never blocks.
    pub fn stop(&self) {
        self.sessions.stop();
    }

    /// Stops the session and waits up to 8 seconds for it to say goodbye to the host.
    /// Blocks, so never call it on the main thread.
    pub fn shut_down(&self) {
        self.sessions.shut_down();
    }
}

impl MobileJoin {
    fn with_peer(
        peer: PeerOptions,
        id: String,
        listener: Box<dyn EventListener>,
    ) -> Result<Self, MobileError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mcrtc")
            .enable_all()
            .build()?;
        let address = Edition::Bedrock.default_address().to_owned();
        let options = UiOptions {
            peer,
            edition: Edition::Bedrock,
            server: address.clone(),
            listen: address,
            max_peers: None,
            max_connections: None,
            whitelist: None,
        };
        // The listener thread blocks on the channel, so nothing needs waking.
        let (sessions, events) = Sessions::new(options, runtime.handle().clone(), Arc::new(|| {}));
        spawn_forwarder(events, listener)?;
        Ok(Self {
            sessions,
            id,
            _runtime: runtime,
        })
    }
}

/// Hands events to Kotlin until the [`Sessions`] and its sessions are gone.
fn spawn_forwarder(
    events: mpsc::Receiver<Event>,
    listener: Box<dyn EventListener>,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("mcrtc-events".to_owned())
        .spawn(move || {
            for event in events {
                listener.on_event(event.into());
            }
        })?;
    Ok(())
}

fn default_ice() -> IceConfig {
    IceConfig {
        stun: DEFAULT_STUN.iter().map(|url| url.to_string()).collect(),
        turn: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
