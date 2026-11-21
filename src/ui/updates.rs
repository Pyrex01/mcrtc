//! The title screen's update button: checks GitHub once when the window opens, then downloads
//! and swaps in the new binary when asked, all off the window's thread.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use semver::Version;

use super::session::Sessions;
use crate::update::{self, Release};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    /// Checking, not checking at all, or already the newest.
    #[default]
    None,
    Available(Release),
    Installing(Version),
    /// Installed. Starting `exe` again runs the new version.
    Installed {
        version: Version,
        exe: PathBuf,
    },
}

#[derive(Default)]
struct Shared {
    status: Status,
    /// An install that went wrong, for the window to show once.
    failure: Option<String>,
}

#[derive(Clone, Default)]
pub struct Updates {
    shared: Arc<Mutex<Shared>>,
}

impl Updates {
    pub fn check(sessions: &Sessions) -> Self {
        let updates = Self::default();
        let shared = updates.shared.clone();
        sessions.background(move || match update::check() {
            Ok(Some(release)) => {
                tracing::info!("mcrtc {} is available", release.version);
                shared.lock().unwrap().status = Status::Available(release);
            }
            Ok(None) => tracing::debug!("mcrtc is up to date"),
            // Offline, rate limited or an unreleased platform: nothing the player needs to see.
            Err(error) => tracing::debug!("could not check for updates: {error:#}"),
        });
        updates
    }

    #[cfg(test)]
    pub fn with(status: Status) -> Self {
        let updates = Self::default();
        updates.shared.lock().unwrap().status = status;
        updates
    }

    pub fn status(&self) -> Status {
        self.shared.lock().unwrap().status.clone()
    }

    pub fn take_failure(&self) -> Option<String> {
        self.shared.lock().unwrap().failure.take()
    }

    pub fn install(&self, sessions: &Sessions) {
        let release = {
            let mut shared = self.shared.lock().unwrap();
            let Status::Available(release) = shared.status.clone() else {
                return;
            };
            shared.status = Status::Installing(release.version.clone());
            release
        };
        let shared = self.shared.clone();
        sessions.background(move || {
            let result = update::install(&release);
            let mut shared = shared.lock().unwrap();
            match result {
                Ok(exe) => {
                    shared.status = Status::Installed {
                        version: release.version,
                        exe,
                    }
                }
                Err(error) => {
                    tracing::warn!("update failed: {error:#}");
                    // The whole chain is in the log; a toast has room for its outline.
                    shared.failure = Some(format!("Update failed: {error}"));
                    shared.status = Status::Available(release);
                }
            }
        });
    }
}
