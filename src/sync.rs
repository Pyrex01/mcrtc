//! World snapshot creation and transfer.
//!
//! Sync is deliberately opt-in.  A snapshot is a compressed tar archive with a small JSON
//! envelope sent first on a dedicated reliable WebRTC data channel.

use std::fs::{self};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use bytes::BytesMut;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::{Archive, Builder};
use tokio::time::timeout;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelState};

use crate::bridge::{BUFFERED_HIGH, BUFFERED_LOW, MAX_CHUNK};

const HEADER_PREFIX: &str = "MCRTC-WORLD-1 ";
const RCON_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct SyncOptions {
    pub world_path: PathBuf,
    pub interval_secs: u64,
    pub rcon_address: Option<String>,
    pub rcon_password: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub generation: u64,
    pub created_unix: u64,
    pub world_name: String,
    pub size: usize,
    pub sha256: String,
}

#[derive(Debug)]
pub struct Snapshot {
    pub header: SnapshotHeader,
    pub archive: Vec<u8>,
}

pub async fn create_snapshot(options: &SyncOptions) -> Result<Snapshot> {
    let options = options.clone();
    tokio::task::spawn_blocking(move || create_snapshot_blocking(&options))
        .await
        .context("snapshot task failed")?
}

fn create_snapshot_blocking(options: &SyncOptions) -> Result<Snapshot> {
    if let (Some(address), Some(password)) = (&options.rcon_address, &options.rcon_password) {
        rcon_command(address, password, "save-off")?;
        let result = rcon_command(address, password, "save-all flush");
        let _ = rcon_command(address, password, "save-on");
        result?;
    }
    let source = &options.world_path;
    ensure_directory(source)?;
    let mut archive = Vec::new();
    {
        let encoder = GzEncoder::new(&mut archive, Compression::default());
        let mut builder = Builder::new(encoder);
        append_world(&mut builder, source, Path::new("world"))?;
        let encoder = builder.into_inner()?;
        encoder.finish()?;
    }
    let mut hasher = Sha256::new();
    hasher.update(&archive);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst).max(now);
    Ok(Snapshot {
        header: SnapshotHeader {
            generation,
            created_unix: generation,
            world_name: source
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("world")
                .into(),
            size: archive.len(),
            sha256: format!("{:x}", hasher.finalize()),
        },
        archive,
    })
}

fn ensure_directory(path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!("world path is not a directory: {}", path.display());
    }
    Ok(())
}

fn append_world<W: Write>(
    builder: &mut Builder<W>,
    source: &Path,
    archive_root: &Path,
) -> Result<()> {
    builder.append_dir(archive_root, source)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "session.lock") {
            continue;
        }
        let target = archive_root.join(entry.file_name());
        if path.is_dir() {
            append_world(builder, &path, &target)?;
        } else if path.is_file() {
            builder.append_path_with_name(&path, target)?;
        }
    }
    Ok(())
}

pub async fn send_snapshot(
    channel: std::sync::Arc<dyn DataChannel>,
    snapshot: Snapshot,
) -> Result<()> {
    timeout(Duration::from_secs(15), async {
        loop {
            if channel.ready_state().await? == RTCDataChannelState::Open {
                break Ok::<_, anyhow::Error>(());
            }
            match channel.poll().await {
                Some(DataChannelEvent::OnClose) | None => {
                    bail!("sync channel closed before opening")
                }
                _ => {}
            }
        }
    })
    .await
    .context("timed out opening sync channel")??;
    let header = serde_json::to_string(&snapshot.header)?;
    channel
        .send_text(&format!("{HEADER_PREFIX}{header}"))
        .await?;
    for chunk in snapshot.archive.chunks(MAX_CHUNK) {
        while channel.outstanding_bytes().await.unwrap_or(0) > BUFFERED_HIGH {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        channel.send(BytesMut::from(chunk)).await?;
    }
    while channel.outstanding_bytes().await.unwrap_or(0) > BUFFERED_LOW {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    channel.close().await?;
    Ok(())
}

pub async fn receive_channel(
    channel: std::sync::Arc<dyn DataChannel>,
    destination: PathBuf,
) -> Result<()> {
    let mut header = None;
    let mut archive = Vec::new();
    while let Some(event) = channel.poll().await {
        match event {
            DataChannelEvent::OnMessage(message) => {
                if header.is_none() {
                    if let Ok(text) = std::str::from_utf8(&message.data) {
                        if let Some(json) = text.strip_prefix(HEADER_PREFIX) {
                            header = Some(serde_json::from_str::<SnapshotHeader>(json)?);
                            continue;
                        }
                    }
                }
                archive.extend_from_slice(&message.data);
            }
            DataChannelEvent::OnClose => break,
            _ => {}
        }
    }
    let header = header.context("world sync channel had no header")?;
    if archive.len() != header.size {
        bail!("world snapshot size mismatch");
    }
    let mut hasher = Sha256::new();
    hasher.update(&archive);
    if format!("{:x}", hasher.finalize()) != header.sha256 {
        bail!("world snapshot checksum mismatch");
    }
    install_snapshot(&destination, &archive).await
}

async fn install_snapshot(destination: &Path, archive: &[u8]) -> Result<()> {
    let destination = destination.to_owned();
    let bytes = archive.to_vec();
    tokio::task::spawn_blocking(move || {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let staging = parent.join(format!(".mcrtc-world-{}.tmp", std::process::id()));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        let decoder = GzDecoder::new(Cursor::new(bytes));
        let mut archive = Archive::new(decoder);
        archive.unpack(&staging)?;
        let extracted = staging.join("world");
        ensure_directory(&extracted)?;
        if destination.exists() {
            let backup = destination.with_extension(format!("backup-{}", unix_now()));
            fs::rename(&destination, backup)?;
        }
        fs::rename(&extracted, &destination)?;
        fs::remove_dir_all(staging)?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("world install task failed")??;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Minimal Source RCON client: authenticate, issue one command, and require a response.
fn rcon_command(address: &str, password: &str, command: &str) -> Result<()> {
    let mut stream = std::net::TcpStream::connect_timeout(&address.parse()?, RCON_TIMEOUT)?;
    stream.set_read_timeout(Some(RCON_TIMEOUT))?;
    stream.set_write_timeout(Some(RCON_TIMEOUT))?;
    let auth = rcon_packet(1, 3, password);
    stream.write_all(&auth)?;
    let response = read_rcon_packet(&mut stream)?;
    if response.1 == -1 {
        bail!("RCON authentication failed");
    }
    stream.write_all(&rcon_packet(2, 2, command))?;
    let _ = read_rcon_packet(&mut stream)?;
    Ok(())
}

fn rcon_packet(id: i32, kind: i32, body: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(id.to_le_bytes());
    payload.extend(kind.to_le_bytes());
    payload.extend(body.as_bytes());
    payload.extend([0, 0]);
    let mut out = (payload.len() as i32).to_le_bytes().to_vec();
    out.extend(payload);
    out
}

fn read_rcon_packet(stream: &mut std::net::TcpStream) -> Result<(i32, i32, Vec<u8>)> {
    let mut len = [0; 4];
    stream.read_exact(&mut len)?;
    let len = i32::from_le_bytes(len) as usize;
    if !(10..=1024 * 1024).contains(&len) {
        bail!("invalid RCON response");
    }
    let mut data = vec![0; len];
    stream.read_exact(&mut data)?;
    Ok((
        i32::from_le_bytes(data[0..4].try_into()?),
        i32::from_le_bytes(data[4..8].try_into()?),
        data[8..len.saturating_sub(2)].to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rcon_packet_has_protocol_shape() {
        let packet = rcon_packet(1, 3, "pw");
        assert_eq!(i32::from_le_bytes(packet[0..4].try_into().unwrap()), 12);
        assert_eq!(&packet[12..], b"pw\0\0");
    }
}
