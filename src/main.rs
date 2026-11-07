use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::runtime::Runtime;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

use mcrtc::config::{load_whitelist, CommonArgs, FileConfig, IceConfig};
use mcrtc::console::Console;
use mcrtc::events::Reporter;
use mcrtc::host::{self, HostOptions};
use mcrtc::join::{self, JoinOptions};
use mcrtc::peer::{load_or_create_identity, PeerOptions};
use mcrtc::session::UiOptions;

const IP_WARNING: &str = "Warning: invites and replies contain your IP addresses. \
                          Only share them with people you trust.";

/// Tunnel Minecraft Java and Bedrock traffic over WebRTC data channels.
#[derive(Parser)]
#[command(name = "mcrtc", version, about)]
struct Cli {
    /// With no command, opens the graphical interface
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Share a local Minecraft server with friends
    Host {
        /// Address of the Minecraft server [default: 127.0.0.1:25565, Bedrock 127.0.0.1:19132]
        #[arg(long, value_name = "ADDR")]
        server: Option<String>,

        /// Only accept friends whose mcrtc ID is in this file, one per line
        #[arg(long, value_name = "FILE")]
        whitelist_file: Option<PathBuf>,

        /// Maximum number of friends connected at once
        #[arg(long, value_name = "N")]
        max_peers: Option<usize>,

        /// Maximum number of connections per friend
        #[arg(long, value_name = "N")]
        max_connections: Option<usize>,
    },
    /// Open the graphical interface
    Ui,
    /// Connect to a friend's server
    Join {
        /// Local address Minecraft connects to [default: 127.0.0.1:25565, Bedrock 127.0.0.1:19132]
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,

        /// Keep a stable mcrtc ID in this file, created on first use
        #[arg(long, value_name = "FILE")]
        identity: Option<PathBuf>,

        /// Bedrock: listen on UDP only, for RakNet servers that also accept TCP on their port
        #[arg(long)]
        udp_only: bool,
    },
}

fn init_logging(verbose: u8) {
    let default = match verbose {
        // The libraries are chatty about expected things, like probes arriving before the reply
        // is pasted, and their messages can contain addresses.
        0 => "mcrtc=info",
        1 => "warn,mcrtc=debug",
        _ => "info,mcrtc=trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// The first Ctrl+C shuts down cleanly, the second gives up on that.
fn spawn_ctrl_c() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tx.send_replace(true);
        }
        if tokio::signal::ctrl_c().await.is_ok() {
            std::process::exit(130);
        }
    });
    rx
}

fn run(cli: Cli, runtime: &Runtime) -> Result<()> {
    let _entered = runtime.enter();
    let file = match &cli.common.config {
        Some(path) => FileConfig::load(path)?,
        None => FileConfig::default(),
    };
    let ice = IceConfig::resolve(&cli.common, &file)?;
    let mut peer = PeerOptions::new(ice);
    let raw_json = cli.common.raw_json;
    let edition = cli.common.edition.or(file.edition).unwrap_or_default();
    let default_address = edition.default_address().to_owned();
    println!("{IP_WARNING}");

    match cli.command.unwrap_or(Command::Ui) {
        Command::Ui => {
            if let Some(path) = &file.identity {
                peer.certificate = Some(load_or_create_identity(path)?);
            }
            let options = UiOptions {
                peer,
                edition,
                server: file.server.unwrap_or_else(|| default_address.clone()),
                listen: file.listen.unwrap_or(default_address),
                max_peers: file.max_peers,
                max_connections: file.max_connections,
                whitelist: file
                    .whitelist_file
                    .as_deref()
                    .map(load_whitelist)
                    .transpose()?,
            };
            run_ui(options, runtime)
        }
        Command::Host {
            server,
            whitelist_file,
            max_peers,
            max_connections,
        } => {
            let whitelist_file = whitelist_file.or(file.whitelist_file);
            let options = HostOptions {
                peer,
                edition,
                server: server.or(file.server).unwrap_or(default_address),
                max_peers: max_peers.or(file.max_peers),
                max_connections: max_connections.or(file.max_connections),
                whitelist: whitelist_file.as_deref().map(load_whitelist).transpose()?,
                raw_json,
                reporter: Reporter::terminal(),
            };
            runtime.block_on(host::run(options, Console::from_stdin(), spawn_ctrl_c()))
        }
        Command::Join {
            listen,
            identity,
            udp_only,
        } => {
            if let Some(path) = identity.or(file.identity) {
                peer.certificate = Some(load_or_create_identity(&path)?);
            }
            let options = JoinOptions {
                peer,
                edition,
                listen: listen.or(file.listen).unwrap_or(default_address),
                udp_only,
                nethernet_address: None,
                raw_json,
                reporter: Reporter::terminal(),
            };
            runtime.block_on(join::run(options, Console::from_stdin(), spawn_ctrl_c()))
        }
    }
}

#[cfg(feature = "gui")]
fn run_ui(options: UiOptions, runtime: &Runtime) -> Result<()> {
    mcrtc::ui::run(options, runtime.handle().clone(), spawn_ctrl_c())
}

#[cfg(not(feature = "gui"))]
fn run_ui(_options: UiOptions, _runtime: &Runtime) -> Result<()> {
    anyhow::bail!("this build has no graphical interface; use `mcrtc host` or `mcrtc join`")
}

// Not `#[tokio::main]`: some platforms only allow windows on the main thread, so the interface
// needs that thread to itself while the runtime works on its own.
fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.common.verbose);
    let result = Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| run(cli, &runtime));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
