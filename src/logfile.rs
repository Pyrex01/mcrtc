//! A log file that rotates by size: once `mcrtc.log` reaches the limit it becomes `mcrtc.log.1`,
//! replacing the one before, and a fresh `mcrtc.log` starts. At most two files are ever kept.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
const FILE_NAME: &str = "mcrtc.log";

/// Where logs go unless `MCRTC_LOG_DIR` says otherwise: `%LOCALAPPDATA%\mcrtc\logs` on Windows,
/// `~/Library/Logs/mcrtc` on macOS and `$XDG_STATE_HOME/mcrtc` (or `~/.local/state/mcrtc`)
/// elsewhere.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MCRTC_LOG_DIR") {
        return Some(dir.into());
    }
    let env = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    if cfg!(windows) {
        Some(
            PathBuf::from(env("LOCALAPPDATA")?)
                .join("mcrtc")
                .join("logs"),
        )
    } else if cfg!(target_os = "macos") {
        Some(PathBuf::from(env("HOME")?).join("Library/Logs/mcrtc"))
    } else if let Some(state) = env("XDG_STATE_HOME") {
        Some(PathBuf::from(state).join("mcrtc"))
    } else {
        Some(PathBuf::from(env("HOME")?).join(".local/state/mcrtc"))
    }
}

struct Inner {
    path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
}

impl Inner {
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let previous = self.path.with_extension("log.1");
        // Windows will not rename over an existing file.
        match fs::remove_file(&previous) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        fs::rename(&self.path, &previous)?;
        self.file = File::create(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

#[derive(Clone)]
pub struct RotatingFile(Arc<Mutex<Inner>>);

impl RotatingFile {
    pub fn open(dir: &Path, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self(Arc::new(Mutex::new(Inner {
            path,
            file,
            written,
            max_bytes,
        }))))
    }

    pub fn path(&self) -> PathBuf {
        self.lock().path.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One formatted event. tracing writes each event in a single call, so rotating before the
/// write keeps lines whole and each file under the limit.
pub struct Writer<'a>(MutexGuard<'a, Inner>);

impl Write for Writer<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let inner = &mut self.0;
        if inner.written > 0 && inner.written + buf.len() as u64 > inner.max_bytes {
            inner.rotate()?;
        }
        let n = inner.file.write(buf)?;
        inner.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.file.flush()
    }
}

impl<'a> MakeWriter<'a> for RotatingFile {
    type Writer = Writer<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        Writer(self.lock())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcrtc-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn rotates_once_full_and_keeps_one_previous_file() {
        let dir = temp_dir("rotate");
        let log = RotatingFile::open(&dir, 10).unwrap();
        for line in ["aaaaaa\n", "bbbbbb\n", "cccccc\n"] {
            log.make_writer().write_all(line.as_bytes()).unwrap();
        }
        assert_eq!(
            fs::read_to_string(dir.join("mcrtc.log")).unwrap(),
            "cccccc\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("mcrtc.log.1")).unwrap(),
            "bbbbbb\n"
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn appends_to_an_existing_log_and_counts_its_size() {
        let dir = temp_dir("append");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mcrtc.log"), "old\n").unwrap();
        let log = RotatingFile::open(&dir, 10).unwrap();
        log.make_writer().write_all(b"new\n").unwrap();
        assert_eq!(fs::read_to_string(log.path()).unwrap(), "old\nnew\n");
        log.make_writer().write_all(b"more\n").unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("mcrtc.log.1")).unwrap(),
            "old\nnew\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
