//! World sync: friends keep a copy of the host's Java world, so they can carry on without the
//! host.
//!
//! The host keeps a mirror of the world folder next to it. Every interval, while a friend is
//! syncing, it turns saving off over RCON, flushes, copies the files whose size or modification
//! time changed into the mirror, and turns saving back on. A scan that changed anything is a new
//! generation. Friends say which generation they have and get only the files changed since,
//! read from the mirror, so the live world is only read while saving is off and no one waits on
//! a slow friend. Nothing is compressed: region files already are, and skipping it keeps the CPU
//! idle. Files are streamed in chunks, so memory use does not grow with the world.
//!
//! A friend writes incoming files to a staging folder and moves them into place only once the
//! whole generation arrived, through a journal that is finished on the next start if mcrtc dies
//! halfway. Its copy is therefore always one complete generation. If the copy changed locally,
//! say because they played on it while the host was away, it is backed up and copied again in
//! full instead of being overwritten.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
use flate2::Crc;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch, Notify, RwLock};
use tokio::time::{interval, sleep, timeout, Instant, MissedTickBehavior};
use tracing::{debug, info, warn};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};

use crate::bridge::{Counter, MAX_CHUNK};
use crate::control::{PendingOpens, Transport};
use crate::events::{Event, Reporter};
use crate::peer::Peer;

pub const LABEL: &str = "world";

/// Odd, so it never meets the tunnel channels' even IDs, and below libwebrtc's 1024 streams.
pub const STREAM_ID: u16 = 1001;

/// The control request for the world channel. Tunnel IDs count up from 0 and never get here.
pub const REQUEST_ID: u64 = u64::MAX;

pub const DEFAULT_INTERVAL_SECS: u64 = 300;

const VERSION: u32 = 1;
const DEFAULT_RCON_PORT: u16 = 25575;

/// Kept small, so world files never queue up for long in front of the game's own packets.
const SEND_HIGH: usize = 128 * 1024;
const SEND_LOW: usize = 32 * 1024;
const CAPACITY_RECHECK: Duration = Duration::from_millis(20);

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
/// Covers the friend moving a whole generation into place.
const ACK_TIMEOUT: Duration = Duration::from_secs(120);
/// A stuck disk must not hold up the connection the game shares with this channel.
const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A friend that just connected gets a fresh scan unless there was one this recently.
const FRESH_SCAN: Duration = Duration::from_secs(30);
const RCON_TIMEOUT: Duration = Duration::from_secs(10);
/// A big world can take a while to flush.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(120);
/// How often a file may change under the copy before the scan leaves it for the next one.
const COPY_ATTEMPTS: usize = 3;
/// Failed generations in a row before a friend gives up.
const MAX_FAILURES: usize = 3;

/// Held by the running server, so never part of a copy.
const SESSION_LOCK: &str = "session.lock";

const MANIFEST: &str = "manifest.json";
const STATE: &str = "state.json";
const JOURNAL: &str = "journal.json";
const FILES: &str = "files";
const STAGING: &str = "staging";
const PARTIAL: &str = "partial";

#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// The world folder, or the server folder that holds it.
    pub world_path: PathBuf,
    pub interval_secs: u64,
    pub rcon_address: Option<String>,
    pub rcon_password: Option<String>,
}

/// What a file looked like on disk, to notice that it changed without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    len: u64,
    modified: u64,
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos() as u64);
        Self {
            len: metadata.len(),
            modified,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Message {
    /// Friend: the generation it has. No world asks for a full copy.
    Hello {
        version: u32,
        world: Option<String>,
        generation: u64,
    },
    /// Friend: the generation is in place.
    Ack {
        generation: u64,
    },
    /// Host: a generation follows. A full one replaces everything.
    Begin {
        world: String,
        generation: u64,
        full: bool,
    },
    /// Host: `len` bytes of the file follow in binary messages.
    File {
        path: String,
        len: u64,
        crc: u32,
    },
    Delete {
        path: String,
    },
    End {
        generation: u64,
    },
}

async fn send_message(channel: &dyn DataChannel, message: &Message) -> Result<()> {
    channel.send_text(&serde_json::to_string(message)?).await?;
    Ok(())
}

/// A negotiated channel opens without a handshake, as soon as this side created it.
async fn wait_open(channel: &dyn DataChannel) -> Result<()> {
    let opened = async {
        loop {
            if channel.ready_state().await? == RTCDataChannelState::Open {
                return Ok(());
            }
            match channel.poll().await {
                Some(DataChannelEvent::OnOpen) => return Ok(()),
                Some(DataChannelEvent::OnClose) | None => bail!("the world channel closed"),
                Some(_) => {}
            }
        }
    };
    timeout(OPEN_TIMEOUT, opened)
        .await
        .context("the world channel never opened")?
}

/// Every file under `root` by its `/`-separated relative path.
fn list_files(root: &Path) -> Result<BTreeMap<String, Stamp>> {
    let mut files = BTreeMap::new();
    let mut folders = vec![root.to_owned()];
    while let Some(folder) = folders.pop() {
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            // Deleted while we looked.
            Err(error) if error.kind() == io::ErrorKind::NotFound && folder != root => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", folder.display()))
            }
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                folders.push(path);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Some(relative) = relative_path(root, &path) else {
                warn!(path = %path.display(), "skipping a file whose name is not UTF-8");
                continue;
            };
            if relative == SESSION_LOCK {
                continue;
            }
            match entry.metadata() {
                Ok(metadata) => {
                    files.insert(relative, Stamp::of(&metadata));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(files)
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let parts = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| match part {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(parts.join("/"))
}

/// `root` joined with a path from the other side, which must stay inside `root`.
fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    let inside = !relative.is_empty()
        && !relative.contains('\\')
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
    if !inside || relative == SESSION_LOCK {
        bail!("refusing the path {relative:?}");
    }
    Ok(root.join(path))
}

/// A hidden folder next to `path`, so it lives on the same disk and renames stay cheap.
fn sidecar(path: &Path, suffix: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("{} has no folder name", path.display()))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent.join(format!(".{name}.{suffix}")))
}

fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("{} is damaged", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Written next to the target and renamed over it, so a crash leaves the old or the new one.
fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let partial = path.with_extension("partial");
    let mut file = BufWriter::new(File::create(&partial)?);
    serde_json::to_writer(&mut file, value)?;
    file.into_inner()
        .map_err(|error| error.into_error())?
        .sync_all()?;
    fs::rename(&partial, path)?;
    Ok(())
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000.0
}

// ---------------------------------------------------------------------------------------------
// Host

/// A world file as copied into the mirror.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// The world file when it was copied.
    source: Stamp,
    len: u64,
    crc: u32,
    /// The generation that last changed it.
    generation: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    /// Tells a copy of this mirror from a copy of anything else.
    world: String,
    generation: u64,
    files: BTreeMap<String, Entry>,
    /// When each file that is gone went away.
    deleted: BTreeMap<String, u64>,
}

/// What one scan found.
#[derive(Debug, Default)]
struct Scan {
    changed: usize,
    deleted: usize,
    bytes: u64,
}

/// The shared world of a host: one mirror and one scanner, however many friends sync.
pub struct World {
    source: PathBuf,
    mirror: PathBuf,
    interval: Duration,
    rcon: Option<RconTarget>,
    manifest: RwLock<Manifest>,
    generation: watch::Sender<u64>,
    friends: Counter,
    scan_now: Notify,
    last_scan: Mutex<Option<Instant>>,
}

impl World {
    /// Checks the folder and loads the mirror. Nothing is scanned before [`World::run`].
    pub fn open(options: &SyncOptions) -> Result<Arc<Self>> {
        let source = find_world(&options.world_path)?;
        let mirror = sidecar(&source, "mcrtc-host")?;
        let rcon = match (&options.rcon_address, &options.rcon_password) {
            (address, Some(password)) => Some(RconTarget {
                address: address
                    .clone()
                    .unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_RCON_PORT}")),
                password: password.clone(),
            }),
            (_, None) => rcon_from_properties(&source),
        };
        match &rcon {
            Some(rcon) => info!(address = %rcon.address, "world sync pauses saving over RCON"),
            None => warn!(
                "RCON is not set up, so a snapshot can catch the server halfway through saving. \
                 Set enable-rcon=true and rcon.password in server.properties."
            ),
        }
        let manifest = load_mirror(&mirror)?;
        info!(
            world = %source.display(),
            generation = manifest.generation,
            "sharing the world with friends who ask for it"
        );
        Ok(Arc::new(Self {
            source,
            mirror,
            interval: Duration::from_secs(options.interval_secs.max(1)),
            rcon,
            generation: watch::channel(manifest.generation).0,
            manifest: RwLock::new(manifest),
            friends: Counter::default(),
            scan_now: Notify::new(),
            last_scan: Mutex::new(None),
        }))
    }

    /// Scans every interval while a friend syncs, and when a friend arrives. Never returns.
    pub async fn run(self: Arc<Self>) {
        let mut tick = interval(self.interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                () = self.scan_now.notified() => {}
            }
            if self.friends.get() == 0 {
                continue;
            }
            let world = self.clone();
            let result = tokio::task::spawn_blocking(move || world.scan()).await;
            *self.last_scan.lock().expect("last scan lock") = Some(Instant::now());
            match result {
                Ok(Ok(Some((generation, scan)))) => info!(
                    generation,
                    changed = scan.changed,
                    deleted = scan.deleted,
                    "world snapshot, {:.1} MB copied",
                    megabytes(scan.bytes)
                ),
                Ok(Ok(None)) => debug!("the world has not changed"),
                Ok(Err(error)) => warn!("could not snapshot the world: {error:#}"),
                Err(error) => warn!(%error, "the world snapshot failed"),
            }
        }
    }

    fn request_scan(&self) {
        let recent = self
            .last_scan
            .lock()
            .expect("last scan lock")
            .is_some_and(|last| last.elapsed() < FRESH_SCAN);
        if !recent {
            self.scan_now.notify_one();
        }
    }

    /// Brings the mirror up to date. Returns the new generation, if there is one.
    fn scan(&self) -> Result<Option<(u64, Scan)>> {
        // Taken before saving stops: a friend still reading the mirror must not keep it off.
        let mut manifest = self.manifest.blocking_write();
        let next = manifest.generation + 1;
        let mut scan = Scan::default();
        let paused = self.rcon.as_ref().map(SavePause::start).transpose()?;
        let files = list_files(&self.source)?;
        for (path, stamp) in &files {
            if manifest
                .files
                .get(path)
                .is_some_and(|entry| entry.source == *stamp)
            {
                continue;
            }
            match self.copy(path, next)? {
                Copied::Done(entry) => {
                    scan.changed += 1;
                    scan.bytes += entry.len;
                    manifest.deleted.remove(path);
                    manifest.files.insert(path.clone(), entry);
                }
                // Gone since the listing, which the next scan sees.
                Copied::Gone => {}
                Copied::Busy => warn!(path, "a world file kept changing, trying again next time"),
            }
        }
        drop(paused);

        let gone: Vec<String> = manifest
            .files
            .keys()
            .filter(|path| !files.contains_key(*path))
            .cloned()
            .collect();
        for path in gone {
            manifest.files.remove(&path);
            remove_file(&self.mirror.join(FILES).join(&path))?;
            manifest.deleted.insert(path, next);
            scan.deleted += 1;
        }

        // The first scan always counts, so friends can tell an empty world from no scan yet.
        if scan.changed + scan.deleted == 0 && manifest.generation > 0 {
            return Ok(None);
        }
        manifest.generation = next;
        save_json(&self.mirror.join(MANIFEST), &*manifest)?;
        self.generation.send_replace(next);
        Ok(Some((next, scan)))
    }

    /// Copies one world file into the mirror, making sure it held still meanwhile.
    fn copy(&self, relative: &str, generation: u64) -> Result<Copied> {
        let from = self.source.join(relative);
        let to = self.mirror.join(FILES).join(relative);
        let partial = self.mirror.join(PARTIAL);
        for _ in 0..COPY_ATTEMPTS {
            let before = match fs::metadata(&from) {
                Ok(metadata) => Stamp::of(&metadata),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Copied::Gone),
                Err(error) => return Err(error.into()),
            };
            let (len, crc) = match copy_with_crc(&from, &partial) {
                Ok(copied) => copied,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Copied::Gone),
                Err(error) => {
                    return Err(error).with_context(|| format!("could not copy {relative}"))
                }
            };
            let after = fs::metadata(&from).map(|metadata| Stamp::of(&metadata));
            if after.is_ok_and(|after| after == before) {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(&partial, &to)?;
                return Ok(Copied::Done(Entry {
                    source: before,
                    len,
                    crc,
                    generation,
                }));
            }
        }
        remove_file(&partial)?;
        Ok(Copied::Busy)
    }

    /// Sends one friend the world until the channel closes.
    pub async fn serve(self: Arc<Self>, channel: Arc<dyn DataChannel>, label: String) {
        let _friend = self.friends.acquire();
        let (messages_tx, mut messages) = mpsc::channel(4);
        let low = Notify::new();
        let result = tokio::select! {
            () = host_events(channel.as_ref(), messages_tx, &low) => Ok(()),
            result = self.serve_friend(channel.as_ref(), &mut messages, &low, &label) => result,
        };
        match result {
            Ok(()) => debug!(peer = %label, "world channel closed"),
            Err(error) => warn!(peer = %label, "world sync stopped: {error:#}"),
        }
    }

    async fn serve_friend(
        &self,
        channel: &dyn DataChannel,
        messages: &mut mpsc::Receiver<Message>,
        low: &Notify,
        label: &str,
    ) -> Result<()> {
        channel
            .set_buffered_amount_low_threshold(SEND_LOW as u32)
            .await?;
        let mut changes = self.generation.subscribe();
        let mut have = match timeout(HELLO_TIMEOUT, messages.recv()).await {
            Ok(Some(message)) => Have::from_hello(message)?,
            Ok(None) => return Ok(()),
            Err(_) => bail!("the friend never said which copy it has"),
        };
        self.request_scan();
        changes.wait_for(|generation| *generation > 0).await?;
        let mut buffer = vec![0; MAX_CHUNK];

        loop {
            let (world, generation, sent) = self
                .send_generation(channel, low, &have, &mut buffer)
                .await?;
            match timeout(ACK_TIMEOUT, messages.recv())
                .await
                .context("the friend never confirmed the copy")?
            {
                Some(Message::Ack { generation: acked }) if acked == generation => {
                    if sent.changed + sent.deleted > 0 {
                        info!(
                            peer = %label,
                            generation,
                            files = sent.changed,
                            "sent the world, {:.1} MB",
                            megabytes(sent.bytes)
                        );
                    }
                    have = Have {
                        world: Some(world),
                        generation,
                    };
                }
                // Something went wrong over there, so it starts over from what it has.
                Some(message @ Message::Hello { .. }) => {
                    have = Have::from_hello(message)?;
                    continue;
                }
                Some(other) => bail!("unexpected {other:?}"),
                None => return Ok(()),
            }

            tokio::select! {
                changed = changes.wait_for(|latest| *latest > have.generation) => {
                    changed?;
                }
                message = messages.recv() => match message {
                    Some(message @ Message::Hello { .. }) => have = Have::from_hello(message)?,
                    Some(other) => bail!("unexpected {other:?}"),
                    None => return Ok(()),
                },
            }
        }
    }

    /// Sends what changed since `have`. Returns the world and generation the friend now has.
    async fn send_generation(
        &self,
        channel: &dyn DataChannel,
        low: &Notify,
        have: &Have,
        buffer: &mut [u8],
    ) -> Result<(String, u64, Scan)> {
        let (world, generation, full, files, deleted) = {
            let manifest = self.manifest.read().await;
            let full = have.world.as_deref() != Some(manifest.world.as_str())
                || have.generation > manifest.generation;
            let since = if full { 0 } else { have.generation };
            let files: Vec<String> = manifest
                .files
                .iter()
                .filter(|(_, entry)| entry.generation > since)
                .map(|(path, _)| path.clone())
                .collect();
            let deleted: Vec<String> = match full {
                true => Vec::new(),
                false => manifest
                    .deleted
                    .iter()
                    .filter(|(_, generation)| **generation > since)
                    .map(|(path, _)| path.clone())
                    .collect(),
            };
            let world = manifest.world.clone();
            (world, manifest.generation, full, files, deleted)
        };

        let begin = Message::Begin {
            world: world.clone(),
            generation,
            full,
        };
        send_message(channel, &begin).await?;
        let mut sent = Scan::default();
        for path in files {
            match self.send_file(channel, low, &path, buffer).await? {
                Some(len) => {
                    sent.changed += 1;
                    sent.bytes += len;
                }
                None => sent.deleted += 1,
            }
        }
        for path in deleted {
            send_message(channel, &Message::Delete { path }).await?;
            sent.deleted += 1;
        }
        send_message(channel, &Message::End { generation }).await?;
        Ok((world, generation, sent))
    }

    /// Streams one file from the mirror. A scan may have changed it since the generation began,
    /// which is fine: what goes out is newer, and the next generation covers it again.
    async fn send_file(
        &self,
        channel: &dyn DataChannel,
        low: &Notify,
        path: &str,
        buffer: &mut [u8],
    ) -> Result<Option<u64>> {
        // Held for the file, so the scan cannot replace it halfway through.
        let manifest = self.manifest.read().await;
        let Some(entry) = manifest.files.get(path) else {
            let path = path.to_owned();
            send_message(channel, &Message::Delete { path }).await?;
            return Ok(None);
        };
        let mut file = tokio::fs::File::open(self.mirror.join(FILES).join(path))
            .await
            .with_context(|| format!("the mirror lost {path}"))?;
        let header = Message::File {
            path: path.to_owned(),
            len: entry.len,
            crc: entry.crc,
        };
        send_message(channel, &header).await?;
        let mut remaining = entry.len;
        while remaining > 0 {
            wait_for_room(channel, low).await?;
            let len = remaining.min(buffer.len() as u64) as usize;
            let chunk = &mut buffer[..len];
            file.read_exact(chunk).await?;
            channel.send(BytesMut::from(&chunk[..])).await?;
            remaining -= chunk.len() as u64;
        }
        Ok(Some(entry.len))
    }
}

enum Copied {
    Done(Entry),
    Gone,
    Busy,
}

/// The copy a friend says it has.
struct Have {
    world: Option<String>,
    generation: u64,
}

impl Have {
    fn from_hello(message: Message) -> Result<Self> {
        match message {
            Message::Hello {
                version: VERSION,
                world,
                generation,
            } => Ok(Self { world, generation }),
            Message::Hello { version, .. } => {
                bail!("the friend speaks world sync version {version}, this is {VERSION}")
            }
            other => bail!("expected a hello, got {other:?}"),
        }
    }
}

/// Forwards the friend's messages and keeps the channel's events drained while files go out.
async fn host_events(channel: &dyn DataChannel, messages: mpsc::Sender<Message>, low: &Notify) {
    while let Some(event) = channel.poll().await {
        match event {
            DataChannelEvent::OnMessage(message) => match serde_json::from_slice(&message.data) {
                Ok(message) => {
                    if messages.send(message).await.is_err() {
                        return;
                    }
                }
                Err(error) => debug!(%error, "ignoring a world sync message"),
            },
            DataChannelEvent::OnBufferedAmountLow => low.notify_one(),
            DataChannelEvent::OnClose => return,
            _ => {}
        }
    }
}

/// Like the bridge's: the low event can be late, so the counter is also checked on a timer.
async fn wait_for_room(channel: &dyn DataChannel, low: &Notify) -> Result<()> {
    if channel.outstanding_bytes().await? <= SEND_HIGH {
        return Ok(());
    }
    loop {
        tokio::select! {
            () = low.notified() => {}
            () = sleep(CAPACITY_RECHECK) => {}
        }
        if channel.outstanding_bytes().await? <= SEND_LOW {
            return Ok(());
        }
    }
}

/// Accepts the world folder itself or the server folder holding it.
fn find_world(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("could not find the world folder {}", path.display()))?;
    if path.join("level.dat").is_file() {
        return Ok(path);
    }
    if let Some(properties) = read_properties(&path.join("server.properties")) {
        let name = properties.get("level-name").map_or("world", String::as_str);
        let world = path.join(name);
        if world.join("level.dat").is_file() {
            return Ok(world);
        }
    }
    bail!(
        "{} is not a Minecraft world, it has no level.dat",
        path.display()
    )
}

/// Loads the mirror, forgetting any file whose copy went missing so the next scan redoes it.
fn load_mirror(mirror: &Path) -> Result<Manifest> {
    fs::create_dir_all(mirror.join(FILES))
        .with_context(|| format!("could not create {}", mirror.display()))?;
    remove_file(&mirror.join(PARTIAL))?;
    let mut manifest = match load_json::<Manifest>(&mirror.join(MANIFEST)) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => Manifest {
            world: format!("{:016x}", rand::random::<u64>()),
            generation: 0,
            files: BTreeMap::new(),
            deleted: BTreeMap::new(),
        },
        Err(error) => {
            warn!("starting the mirror over: {error:#}");
            fs::remove_dir_all(mirror)?;
            return load_mirror(mirror);
        }
    };
    manifest.files.retain(|path, entry| {
        fs::metadata(mirror.join(FILES).join(path)).is_ok_and(|copy| copy.len() == entry.len)
    });
    Ok(manifest)
}

fn copy_with_crc(from: &Path, to: &Path) -> io::Result<(u64, u32)> {
    let mut source = File::open(from)?;
    let mut target = File::create(to)?;
    let mut crc = Crc::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        crc.update(&buffer[..read]);
        target.write_all(&buffer[..read])?;
    }
    target.sync_all()?;
    Ok((crc.amount() as u64, crc.sum()))
}

fn read_properties(path: &Path) -> Option<BTreeMap<String, String>> {
    let text = fs::read_to_string(path).ok()?;
    let properties = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.starts_with('!'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect();
    Some(properties)
}

/// The server's own RCON settings, when `server.properties` sits next to the world.
fn rcon_from_properties(world: &Path) -> Option<RconTarget> {
    let properties = read_properties(&world.parent()?.join("server.properties"))?;
    if properties.get("enable-rcon").map(String::as_str) != Some("true") {
        return None;
    }
    let password = properties.get("rcon.password").filter(|p| !p.is_empty())?;
    let port = properties
        .get("rcon.port")
        .and_then(|port| port.parse().ok())
        .unwrap_or(DEFAULT_RCON_PORT);
    Some(RconTarget {
        address: format!("127.0.0.1:{port}"),
        password: password.clone(),
    })
}

#[derive(Debug, Clone)]
struct RconTarget {
    address: String,
    password: String,
}

/// Keeps the server from saving while it lives, so the files hold still while they are copied.
struct SavePause {
    rcon: Rcon,
    target: RconTarget,
    /// False when someone had turned saving off on purpose, so it stays off.
    resume: bool,
}

impl SavePause {
    fn start(target: &RconTarget) -> Result<Self> {
        let mut rcon = Rcon::connect(target)?;
        let reply = rcon.command("save-off", RCON_TIMEOUT)?;
        let mut pause = Self {
            rcon,
            target: target.clone(),
            resume: !reply.contains("already"),
        };
        // Saving resumes on drop, even when the flush fails.
        pause.rcon.command("save-all flush", FLUSH_TIMEOUT)?;
        Ok(pause)
    }
}

impl Drop for SavePause {
    fn drop(&mut self) {
        if !self.resume {
            return;
        }
        // A connection that broke during the copy gets one fresh try.
        let resumed = self.rcon.command("save-on", RCON_TIMEOUT).is_ok()
            || Rcon::connect(&self.target)
                .and_then(|mut rcon| rcon.command("save-on", RCON_TIMEOUT))
                .is_ok();
        if !resumed {
            warn!("could not turn saving back on. Run save-on on the server!");
        }
    }
}

/// A minimal Source RCON client, as Minecraft speaks it.
struct Rcon {
    stream: TcpStream,
    next_id: i32,
}

const RCON_AUTH: i32 = 3;
const RCON_COMMAND: i32 = 2;

impl Rcon {
    fn connect(target: &RconTarget) -> Result<Self> {
        let address = &target.address;
        let mut last = None;
        let mut stream = None;
        for candidate in address
            .to_socket_addrs()
            .with_context(|| format!("bad RCON address {address}"))?
        {
            match TcpStream::connect_timeout(&candidate, RCON_TIMEOUT) {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(error) => last = Some(error),
            }
        }
        let stream = stream.ok_or_else(|| {
            let reason = last.map_or("no address".to_owned(), |error| error.to_string());
            anyhow!("could not reach RCON at {address} ({reason}). Is the server running?")
        })?;
        stream.set_write_timeout(Some(RCON_TIMEOUT))?;
        let mut rcon = Self { stream, next_id: 1 };
        let id = rcon.write(RCON_AUTH, &target.password)?;
        rcon.stream.set_read_timeout(Some(RCON_TIMEOUT))?;
        loop {
            let (reply, _) = rcon.read()?;
            if reply == -1 {
                bail!("the server refused the RCON password");
            }
            if reply == id {
                return Ok(rcon);
            }
        }
    }

    fn command(&mut self, command: &str, wait: Duration) -> Result<String> {
        self.stream.set_read_timeout(Some(wait))?;
        let id = self.write(RCON_COMMAND, command)?;
        loop {
            let (reply, body) = self
                .read()
                .with_context(|| format!("no answer to the RCON command {command}"))?;
            // Answers to earlier commands can still be in the stream.
            if reply == id {
                return Ok(body);
            }
        }
    }

    fn write(&mut self, kind: i32, body: &str) -> Result<i32> {
        let id = self.next_id;
        self.next_id += 1;
        self.stream.write_all(&rcon_packet(id, kind, body))?;
        Ok(id)
    }

    fn read(&mut self) -> Result<(i32, String)> {
        let mut len = [0; 4];
        self.stream.read_exact(&mut len)?;
        let len = i32::from_le_bytes(len);
        if !(10..=1024 * 1024).contains(&len) {
            bail!("the RCON answer is not RCON");
        }
        let mut packet = vec![0; len as usize];
        self.stream.read_exact(&mut packet)?;
        let id = i32::from_le_bytes(packet[0..4].try_into()?);
        let body = String::from_utf8_lossy(&packet[8..packet.len() - 2]).into_owned();
        Ok((id, body))
    }
}

fn rcon_packet(id: i32, kind: i32, body: &str) -> Vec<u8> {
    let len = (4 + 4 + body.len() + 2) as i32;
    let mut packet = Vec::with_capacity(4 + len as usize);
    packet.extend(len.to_le_bytes());
    packet.extend(id.to_le_bytes());
    packet.extend(kind.to_le_bytes());
    packet.extend(body.as_bytes());
    packet.extend([0, 0]);
    packet
}

// ---------------------------------------------------------------------------------------------
// Friend

/// The friend's record of its copy.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct LocalState {
    world: Option<String>,
    generation: u64,
    /// What each file looked like after being put in place, to notice local changes.
    files: BTreeMap<String, Stamp>,
}

/// A generation that arrived completely, about to be moved into place.
#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    world: String,
    generation: u64,
    full: bool,
    moves: Vec<String>,
    deletes: Vec<String>,
}

/// The friend's copy of the host's world.
struct Replica {
    dest: PathBuf,
    meta: PathBuf,
    state: LocalState,
}

impl Replica {
    /// Finishes an interrupted generation and checks the copy is still as it was left.
    fn open(dest: PathBuf) -> Result<Self> {
        let dest = std::path::absolute(&dest)?;
        let meta = sidecar(&dest, "mcrtc-sync")?;
        fs::create_dir_all(&meta)
            .with_context(|| format!("could not create {}", meta.display()))?;
        let mut state = load_json(&meta.join(STATE))?.unwrap_or_default();
        if let Some(journal) = load_json::<Journal>(&meta.join(JOURNAL))? {
            info!(
                generation = journal.generation,
                "finishing the last world sync"
            );
            apply(&dest, &meta, &mut state, &journal)?;
        }
        let _ = fs::remove_dir_all(meta.join(STAGING));
        if state.world.is_some() {
            let now = match dest.is_dir() {
                true => list_files(&dest)?,
                false => BTreeMap::new(),
            };
            if now != state.files {
                warn!(
                    "the world copy changed since the last sync, so it will be backed up and \
                     copied again"
                );
                state = LocalState::default();
            }
        }
        Ok(Self { dest, meta, state })
    }

    fn hello(&self) -> Message {
        Message::Hello {
            version: VERSION,
            world: self.state.world.clone(),
            generation: self.state.generation,
        }
    }

    fn staging(&self) -> PathBuf {
        self.meta.join(STAGING)
    }
}

/// A generation on its way in.
struct Incoming {
    world: String,
    generation: u64,
    full: bool,
    moves: Vec<String>,
    deletes: Vec<String>,
    bytes: u64,
    file: Option<Receiving>,
}

struct Receiving {
    path: String,
    file: tokio::fs::File,
    remaining: u64,
    expected: u32,
    crc: Crc,
}

/// Commits `journal`: the old copy is backed up first if it belongs to another world.
fn commit(
    dest: &Path,
    meta: &Path,
    mut state: LocalState,
    journal: &Journal,
) -> Result<LocalState> {
    if state.world.as_deref() != Some(journal.world.as_str()) {
        back_up(dest)?;
        state = LocalState::default();
    }
    save_json(&meta.join(JOURNAL), journal)?;
    apply(dest, meta, &mut state, journal)?;
    Ok(state)
}

/// Moves a journal's files into place. Safe to repeat after a crash.
fn apply(dest: &Path, meta: &Path, state: &mut LocalState, journal: &Journal) -> Result<()> {
    // A crash after the backup left the old world's record behind.
    if state.world.as_deref() != Some(journal.world.as_str()) {
        *state = LocalState::default();
    }
    let staging = meta.join(STAGING);
    fs::create_dir_all(dest)?;
    for path in &journal.moves {
        let from = safe_join(&staging, path)?;
        let to = safe_join(dest, path)?;
        if from.exists() {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&from, &to).with_context(|| format!("could not put {path} in place"))?;
        }
        match fs::metadata(&to) {
            Ok(metadata) => {
                state.files.insert(path.clone(), Stamp::of(&metadata));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                state.files.remove(path);
            }
            Err(error) => return Err(error.into()),
        }
    }
    for path in &journal.deletes {
        remove_file(&safe_join(dest, path)?)?;
        state.files.remove(path);
    }
    if journal.full {
        let keep: HashSet<&str> = journal.moves.iter().map(String::as_str).collect();
        for path in list_files(dest)?.keys() {
            if !keep.contains(path.as_str()) {
                remove_file(&dest.join(path))?;
            }
        }
        state.files.retain(|path, _| keep.contains(path.as_str()));
    }
    state.world = Some(journal.world.clone());
    state.generation = journal.generation;
    save_json(&meta.join(STATE), state)?;
    remove_file(&meta.join(JOURNAL))?;
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

/// Moves a non-empty folder aside, so nothing of it is lost.
fn back_up(dest: &Path) -> Result<()> {
    let has_files = fs::read_dir(dest).is_ok_and(|mut entries| entries.next().is_some());
    if !has_files {
        return Ok(());
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world");
    let backup = dest.with_file_name(format!("{name}.backup-{seconds}"));
    fs::rename(dest, &backup).with_context(|| {
        format!(
            "could not back up {}. Is Minecraft using it?",
            dest.display()
        )
    })?;
    info!(backup = %backup.display(), "backed up the old world copy");
    Ok(())
}

/// Asks the host for its world and keeps `dest` in step with it until the session ends.
pub async fn follow(
    peer: &Peer,
    control: &dyn DataChannel,
    pending: &PendingOpens,
    dest: PathBuf,
    reporter: &Reporter,
) {
    if let Err(error) = follow_inner(peer, control, pending, dest, reporter).await {
        warn!("world sync stopped: {error:#}");
    }
}

async fn follow_inner(
    peer: &Peer,
    control: &dyn DataChannel,
    pending: &PendingOpens,
    dest: PathBuf,
    reporter: &Reporter,
) -> Result<()> {
    let mut replica = tokio::task::spawn_blocking(move || Replica::open(dest)).await??;
    let channel = peer.create_negotiated_channel(LABEL, STREAM_ID).await?;
    wait_open(channel.as_ref()).await?;
    if !pending.request(control, REQUEST_ID, Transport::World).await {
        let _ = channel.close().await;
        bail!("the host is not sharing its world");
    }
    info!(copy = %replica.dest.display(), "keeping a copy of the host's world");
    send_message(channel.as_ref(), &replica.hello()).await?;

    let mut incoming = None;
    let mut failures = 0;
    while let Some(event) = channel.poll().await {
        let message = match event {
            DataChannelEvent::OnMessage(message) => message,
            DataChannelEvent::OnClose => break,
            _ => continue,
        };
        match receive(&mut replica, &mut incoming, &message.data).await {
            Ok(Some(done)) => {
                failures = 0;
                let ack = Message::Ack {
                    generation: replica.state.generation,
                };
                send_message(channel.as_ref(), &ack).await?;
                reporter.report(Event::WorldSynced {
                    files: done.0,
                    bytes: done.1,
                });
            }
            Ok(None) => {}
            Err(error) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    return Err(error);
                }
                warn!("world sync failed, starting over: {error:#}");
                // Whatever is left of this generation is ignored until the next one begins.
                incoming = None;
                let dest = replica.dest.clone();
                replica = tokio::task::spawn_blocking(move || Replica::open(dest)).await??;
                send_message(channel.as_ref(), &replica.hello()).await?;
            }
        }
    }
    Ok(())
}

/// Handles one message. Returns the files and bytes of a generation once it is in place.
async fn receive(
    replica: &mut Replica,
    incoming: &mut Option<Incoming>,
    data: &[u8],
) -> Result<Option<(usize, u64)>> {
    if let Some(receiving) = incoming
        .as_mut()
        .and_then(|incoming| incoming.file.as_mut())
    {
        if data.len() as u64 > receiving.remaining {
            bail!("{} is longer than announced", receiving.path);
        }
        timeout(WRITE_STALL_TIMEOUT, receiving.file.write_all(data))
            .await
            .context("the disk stopped responding")??;
        receiving.crc.update(data);
        receiving.remaining -= data.len() as u64;
        if receiving.remaining == 0 {
            finish_file(incoming.as_mut().expect("incoming generation")).await?;
        }
        return Ok(None);
    }

    let Ok(message) = serde_json::from_slice::<Message>(data) else {
        // The rest of a generation abandoned after an error.
        if incoming.is_some() {
            bail!("unexpected data between files");
        }
        return Ok(None);
    };
    match message {
        Message::Begin {
            world,
            generation,
            full,
        } => {
            let staging = replica.staging();
            if let Err(error) = tokio::fs::remove_dir_all(&staging).await {
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(error.into());
                }
            }
            tokio::fs::create_dir_all(&staging).await?;
            *incoming = Some(Incoming {
                world,
                generation,
                full,
                moves: Vec::new(),
                deletes: Vec::new(),
                bytes: 0,
                file: None,
            });
        }
        Message::File { path, len, crc } => {
            let Some(generation) = incoming.as_mut() else {
                return Ok(None);
            };
            let target = safe_join(&replica.staging(), &path)?;
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let file = tokio::fs::File::create(&target).await?;
            generation.file = Some(Receiving {
                path,
                file,
                remaining: len,
                expected: crc,
                crc: Crc::new(),
            });
            if len == 0 {
                finish_file(generation).await?;
            }
        }
        Message::Delete { path } => {
            let Some(generation) = incoming.as_mut() else {
                return Ok(None);
            };
            safe_join(&replica.dest, &path)?;
            generation.deletes.push(path);
        }
        Message::End { generation } => {
            let Some(done) = incoming.take() else {
                return Ok(None);
            };
            if done.generation != generation {
                bail!("generation {} ended as {generation}", done.generation);
            }
            let journal = Journal {
                world: done.world,
                generation,
                full: done.full,
                moves: done.moves,
                deletes: done.deletes,
            };
            let (dest, meta) = (replica.dest.clone(), replica.meta.clone());
            let state = std::mem::take(&mut replica.state);
            let committed = tokio::task::spawn_blocking(move || {
                commit(&dest, &meta, state, &journal).map(|s| (s, journal))
            })
            .await??;
            let (state, journal) = committed;
            replica.state = state;
            info!(
                generation,
                files = journal.moves.len(),
                "world copy updated, {:.1} MB",
                megabytes(done.bytes)
            );
            return Ok(Some((journal.moves.len(), done.bytes)));
        }
        Message::Hello { .. } | Message::Ack { .. } => {}
    }
    Ok(None)
}

async fn finish_file(generation: &mut Incoming) -> Result<()> {
    let mut receiving = generation.file.take().expect("a file being received");
    receiving.file.flush().await?;
    receiving.file.sync_all().await?;
    if receiving.crc.sum() != receiving.expected {
        bail!("{} arrived damaged", receiving.path);
    }
    generation.bytes += receiving.crc.amount() as u64;
    generation.moves.push(receiving.path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rcon_packets_have_the_protocol_shape() {
        let packet = rcon_packet(1, RCON_AUTH, "pw");
        assert_eq!(i32::from_le_bytes(packet[0..4].try_into().unwrap()), 12);
        assert_eq!(i32::from_le_bytes(packet[4..8].try_into().unwrap()), 1);
        assert_eq!(
            i32::from_le_bytes(packet[8..12].try_into().unwrap()),
            RCON_AUTH
        );
        assert_eq!(&packet[12..], b"pw\0\0");
    }

    fn stage(replica: &Replica, path: &str, contents: &[u8]) {
        let staged = replica.staging().join(path);
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(staged, contents).unwrap();
    }

    fn journal(generation: u64, full: bool, moves: &[&str], deletes: &[&str]) -> Journal {
        Journal {
            world: "w".into(),
            generation,
            full,
            moves: moves.iter().map(|path| path.to_string()).collect(),
            deletes: deletes.iter().map(|path| path.to_string()).collect(),
        }
    }

    #[test]
    fn a_copy_survives_a_crash_and_local_changes_are_backed_up() {
        let root = std::env::temp_dir().join(format!("mcrtc-replica-{:x}", rand::random::<u64>()));
        let dest = root.join("copy");

        let mut replica = Replica::open(dest.clone()).unwrap();
        assert_eq!(replica.state.world, None);
        stage(&replica, "region/a.mca", b"one");
        stage(&replica, "level.dat", b"level");
        let first = journal(1, true, &["region/a.mca", "level.dat"], &[]);
        let state = std::mem::take(&mut replica.state);
        replica.state = commit(&dest, &replica.meta, state, &first).unwrap();
        assert_eq!(fs::read(dest.join("region/a.mca")).unwrap(), b"one");

        // mcrtc died after writing the journal: the next start finishes the job.
        stage(&replica, "region/b.mca", b"two");
        save_json(
            &replica.meta.join(JOURNAL),
            &journal(2, false, &["region/b.mca"], &["level.dat"]),
        )
        .unwrap();
        let replica = Replica::open(dest.clone()).unwrap();
        assert_eq!(replica.state.generation, 2);
        assert_eq!(replica.state.world.as_deref(), Some("w"));
        assert_eq!(fs::read(dest.join("region/b.mca")).unwrap(), b"two");
        assert!(!dest.join("level.dat").exists());
        assert!(!replica.meta.join(JOURNAL).exists());

        // Someone played on the copy: it must be kept, not overwritten.
        fs::write(dest.join("region/a.mca"), b"played on").unwrap();
        let mut replica = Replica::open(dest.clone()).unwrap();
        assert_eq!(
            replica.state.world, None,
            "a changed copy asks for a full one"
        );
        stage(&replica, "region/a.mca", b"host");
        let state = std::mem::take(&mut replica.state);
        commit(
            &dest,
            &replica.meta,
            state,
            &journal(3, true, &["region/a.mca"], &[]),
        )
        .unwrap();
        assert_eq!(fs::read(dest.join("region/a.mca")).unwrap(), b"host");
        assert!(!dest.join("region/b.mca").exists());
        let backup = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().contains("copy.backup-"))
            .expect("a backup of the old copy");
        assert_eq!(fs::read(backup.join("region/a.mca")).unwrap(), b"played on");
        let _ = fs::remove_dir_all(&root);
    }

    /// Against a real server folder with RCON in its `server.properties`:
    /// `MCRTC_TEST_SERVER=/path/to/server cargo test -- --ignored real_server`
    #[test]
    #[ignore]
    fn real_server_snapshot_pauses_and_resumes_saving() {
        let folder = PathBuf::from(std::env::var("MCRTC_TEST_SERVER").unwrap());
        let world = World::open(&SyncOptions {
            world_path: folder,
            interval_secs: 60,
            rcon_address: None,
            rcon_password: None,
        })
        .unwrap();
        let rcon = world.rcon.clone().expect("RCON found in server.properties");
        let (generation, scan) = world.scan().unwrap().expect("a first generation");
        assert_eq!(generation, world.manifest.blocking_read().generation);
        assert!(scan.changed > 0 && scan.bytes > 0, "{scan:?}");
        let reply = Rcon::connect(&rcon)
            .unwrap()
            .command("save-on", RCON_TIMEOUT)
            .unwrap();
        assert!(reply.contains("already"), "saving was left off: {reply}");
        // A flush rewrites level.dat and the like, but the regions nobody visited stay put.
        if let Some((_, again)) = world.scan().unwrap() {
            assert!(again.changed < scan.changed, "{again:?} after {scan:?}");
        }
    }

    #[test]
    fn paths_from_the_other_side_stay_inside() {
        let root = Path::new("/w");
        assert_eq!(
            safe_join(root, "region/r.0.0.mca").unwrap(),
            root.join("region/r.0.0.mca")
        );
        for bad in [
            "",
            "../x",
            "/etc/passwd",
            "a/../../x",
            "a\\..\\x",
            "./a",
            "session.lock",
        ] {
            assert!(safe_join(root, bad).is_err(), "{bad} should be refused");
        }
    }
}
