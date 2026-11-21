// Release builds on Windows are GUI programs, so double-clicking mcrtc opens only its window and
// no terminal beside it. The command-line modes attach to a console in `attach_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod bridge;
mod config;
mod console;
mod control;
mod datagram;
mod edition;
mod events;
mod host;
mod invite;
mod join;
mod logfile;
#[cfg(test)]
mod loopback_tests;
mod peer;
mod sync;
mod ui;
mod update;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::runtime::Runtime;
use tokio::sync::watch;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

use config::{load_whitelist, CommonArgs, FileConfig, IceConfig};
use console::Console;
use events::Reporter;
use host::HostOptions;
use join::JoinOptions;
use peer::{load_or_create_identity, PeerOptions};
use sync::SyncOptions;
use ui::UiOptions;

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

        /// Let friends keep a copy of this Java world, or of the server folder's world
        #[arg(long, value_name = "DIR")]
        world: Option<PathBuf>,

        /// Seconds between world snapshots [default: 300]
        #[arg(long, value_name = "SECONDS")]
        sync_interval: Option<u64>,

        /// RCON address for pausing saves [default: from server.properties, or 127.0.0.1:25575]
        #[arg(long, value_name = "ADDR")]
        rcon_address: Option<String>,

        /// RCON password [default: from server.properties]
        #[arg(long, value_name = "PASS")]
        rcon_password: Option<String>,
    },
    /// Open the graphical interface
    Ui,
    /// Replace this binary with the newest GitHub release
    Update {
        /// Only report whether a newer release exists
        #[arg(long)]
        check: bool,
    },
    /// Connect to a friend's server
    Join {
        /// Local address Minecraft connects to [default: 127.0.0.1:25565, Bedrock 127.0.0.1:19132]
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,

        /// Keep a stable mcrtc ID in this file, created on first use
        #[arg(long, value_name = "FILE")]
        identity: Option<PathBuf>,

        /// Keep a copy of the host's world in this folder
        #[arg(long, value_name = "DIR")]
        sync_path: Option<PathBuf>,
    },
}

/// Fatal errors are printed plainly, so the file is the only log they go to.
const EXIT_TARGET: &str = "mcrtc::exit";

/// Logs go to stderr and to a size-rotated file, so problems in the interface, which has no
/// terminal to print to, can still be looked at afterwards.
fn init_logging(verbose: u8) {
    let default = match verbose {
        // The libraries are chatty about expected things, like probes arriving before the reply
        // is pasted, and their messages can contain addresses.
        0 => "mcrtc=info",
        1 => "warn,mcrtc=debug",
        _ => "info,mcrtc=trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let (file, file_error) = match logfile::default_dir()
        .map(|dir| logfile::RotatingFile::open(&dir, logfile::MAX_BYTES))
    {
        Some(Ok(file)) => (Some(file), None),
        Some(Err(error)) => (None, Some(error.to_string())),
        None => (None, Some("no home or app data folder".to_owned())),
    };
    let path = file.as_ref().map(logfile::RotatingFile::path);
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter_fn(|meta| meta.target() != EXIT_TARGET)),
        )
        .with(file.map(|file| fmt::layer().with_ansi(false).with_writer(file)))
        .init();
    match (path, file_error) {
        (Some(path), _) => tracing::debug!("logging to {}", path.display()),
        (None, Some(error)) => tracing::warn!("not writing a log file: {error}"),
        (None, None) => {}
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}");
        default_hook(info);
    }));
}

/// A Windows GUI program starts without a console, so the command-line modes borrow the one they
/// were started from, or open their own when started some other way.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_console() {
    use windows_sys::Win32::System::Console::{AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: both only change which console this process is attached to.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_console() {}

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

fn sync_options(
    world: Option<PathBuf>,
    interval: Option<u64>,
    rcon_address: Option<String>,
    rcon_password: Option<String>,
    file: &FileConfig,
) -> Option<SyncOptions> {
    Some(SyncOptions {
        world_path: world.or_else(|| file.world.clone())?,
        interval_secs: interval
            .or(file.sync_interval)
            .unwrap_or(sync::DEFAULT_INTERVAL_SECS),
        rcon_address: rcon_address.or_else(|| file.rcon_address.clone()),
        rcon_password: rcon_password.or_else(|| file.rcon_password.clone()),
    })
}

fn run(cli: Cli, runtime: &Runtime) -> Result<()> {
    if let Some(Command::Update { check }) = cli.command {
        return update::run_command(check);
    }
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
                sync: sync_options(None, None, None, None, &file),
                sync_path: file.sync_path.clone(),
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
                check_updates: file.check_updates.unwrap_or(true),
            };
            ui::run(options, runtime.handle().clone(), spawn_ctrl_c())
        }
        Command::Host {
            server,
            whitelist_file,
            max_peers,
            max_connections,
            world,
            sync_interval,
            rcon_address,
            rcon_password,
        } => {
            let sync = sync_options(world, sync_interval, rcon_address, rcon_password, &file);
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
                sync,
            };
            runtime.block_on(host::run(options, Console::from_stdin(), spawn_ctrl_c()))
        }
        Command::Update { .. } => unreachable!("handled before the config is read"),
        Command::Join {
            listen,
            identity,
            sync_path,
        } => {
            if let Some(path) = identity.or(file.identity) {
                peer.certificate = Some(load_or_create_identity(&path)?);
            }
            let options = JoinOptions {
                peer,
                edition,
                listen: listen.or(file.listen).unwrap_or(default_address),
                raw_json,
                reporter: Reporter::terminal(),
                sync_path: sync_path.or(file.sync_path),
            };
            runtime.block_on(join::run(options, Console::from_stdin(), spawn_ctrl_c()))
        }
    }
}

// Not `#[tokio::main]`: some platforms only allow windows on the main thread, so the interface
// needs that thread to itself while the runtime works on its own.
fn main() -> ExitCode {
    let cli = Cli::try_parse().unwrap_or_else(|error| {
        attach_console();
        error.exit()
    });
    if !matches!(cli.command, None | Some(Command::Ui)) {
        attach_console();
    }
    init_logging(cli.common.verbose);
    let result = Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| run(cli, &runtime));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(target: EXIT_TARGET, "{error:#}");
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
