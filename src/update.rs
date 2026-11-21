//! Updates from GitHub releases: finds the newest release, downloads this platform's binary,
//! checks it against the release's `SHA256SUMS`, and swaps it in for the running one.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, ensure, Context, Result};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const LATEST: &str = "https://api.github.com/repos/Pyrex01/mcrtc/releases/latest";
const CHECKSUMS: &str = "SHA256SUMS";
/// Well above a release binary, and still a bound on what a bad response can make us hold.
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;

/// What the release workflow names this platform's binary, if it builds one.
pub fn asset_name() -> Option<&'static str> {
    match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => Some("mcrtc-linux-x86_64"),
        ("windows", "x86_64") => Some("mcrtc-windows-x86_64.exe"),
        ("macos", "aarch64") => Some("mcrtc-macos-arm64"),
        ("macos", "x86_64") => Some("mcrtc-macos-x86_64"),
        _ => None,
    }
}

fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is semver")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    binary_url: String,
    checksums_url: String,
}

impl Release {
    #[cfg(test)]
    pub fn fake(version: Version) -> Self {
        Self {
            version,
            binary_url: String::new(),
            checksums_url: String::new(),
        }
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// The release if it is newer than `running` and has a binary for this platform.
fn newer_release(json: &str, running: &Version, asset: &str) -> Result<Option<Release>> {
    let release: GithubRelease = serde_json::from_str(json).context("unexpected release format")?;
    let tag = release.tag_name.trim_start_matches('v');
    let version =
        Version::parse(tag).with_context(|| format!("release tag `{}`", release.tag_name))?;
    if version <= *running {
        return Ok(None);
    }
    let url = |name: &str| {
        release
            .assets
            .iter()
            .find(|candidate| candidate.name == name)
            .map(|found| found.browser_download_url.clone())
    };
    let (Some(binary_url), Some(checksums_url)) = (url(asset), url(CHECKSUMS)) else {
        // Not an error: a release can still be uploading, or skip a platform.
        tracing::debug!("release {version} has no {asset} with checksums yet");
        return Ok(None);
    };
    Ok(Some(Release {
        version,
        binary_url,
        checksums_url,
    }))
}

/// The digest `SHA256SUMS` lists for `asset`, in the `sha256sum` output format.
fn expected_digest(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == asset).then(|| digest.to_ascii_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!("mcrtc/", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>> {
    agent
        .get(url)
        .header(
            "Accept",
            "application/vnd.github+json, application/octet-stream",
        )
        .call()
        .with_context(|| format!("could not fetch {url}"))?
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .with_context(|| format!("could not download {url}"))
}

/// Asks GitHub for the newest release. Blocking.
pub fn check() -> Result<Option<Release>> {
    let asset = asset_name().ok_or_else(|| {
        anyhow!(
            "no release builds for {}-{}",
            env::consts::OS,
            env::consts::ARCH
        )
    })?;
    let json = get(&agent(), LATEST)?;
    let json = String::from_utf8(json).context("release info is not UTF-8")?;
    newer_release(&json, &current(), asset)
}

/// Downloads `release` and replaces the running binary with it, returning the path to start it
/// from. Blocking. The running process carries on as the old version until it restarts.
pub fn install(release: &Release) -> Result<PathBuf> {
    let asset = asset_name().expect("check found a binary for this platform");
    // Before replacing: afterwards, Linux reports the running binary as deleted.
    let exe = env::current_exe().context("could not find the running binary")?;
    let agent = agent();
    let sums = String::from_utf8(get(&agent, &release.checksums_url)?)
        .context("SHA256SUMS is not UTF-8")?;
    let expected = expected_digest(&sums, asset)
        .with_context(|| format!("SHA256SUMS does not list {asset}"))?;
    let binary = get(&agent, &release.binary_url)?;
    let actual = hex(&Sha256::digest(&binary));
    ensure!(
        actual == expected,
        "the download of {asset} is corrupt (SHA-256 {actual}, expected {expected})"
    );

    // Next to the binary, so the swap is a rename on the same file system.
    let dir = exe.parent().context("the running binary has no folder")?;
    let staged = dir.join(format!(".mcrtc-update-{}", std::process::id()));
    fs::write(&staged, &binary).with_context(|| {
        format!(
            "could not write to {} (is the folder writable?)",
            dir.display()
        )
    })?;
    let swapped = self_replace::self_replace(&staged)
        .with_context(|| format!("could not replace {}", exe.display()));
    let _ = fs::remove_file(&staged);
    swapped?;
    tracing::info!("updated to mcrtc {}", release.version);
    Ok(exe)
}

/// Starts the binary at `exe` with this process's arguments. The caller then exits.
pub fn restart(exe: &Path) -> Result<()> {
    Command::new(exe)
        .args(env::args_os().skip(1))
        .spawn()
        .with_context(|| format!("could not start {}", exe.display()))?;
    Ok(())
}

/// `mcrtc update`: reports or installs the newest release.
pub fn run_command(check_only: bool) -> Result<()> {
    println!("mcrtc {} is running. Checking for updates...", current());
    let Some(release) = check()? else {
        println!("This is the latest version.");
        return Ok(());
    };
    if check_only {
        println!(
            "mcrtc {} is available. Run `mcrtc update` to install it.",
            release.version
        );
        return Ok(());
    }
    println!("Downloading mcrtc {}...", release.version);
    install(&release)?;
    println!("Updated to mcrtc {}.", release.version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_json(tag: &str, assets: &[&str]) -> String {
        let assets: Vec<_> = assets
            .iter()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "browser_download_url": format!("https://example.com/{tag}/{name}"),
                })
            })
            .collect();
        serde_json::json!({ "tag_name": tag, "assets": assets, "draft": false }).to_string()
    }

    #[test]
    fn finds_a_newer_release() {
        let json = release_json("v0.6.0", &["mcrtc-linux-x86_64", CHECKSUMS]);
        let release = newer_release(&json, &Version::new(0, 5, 0), "mcrtc-linux-x86_64")
            .unwrap()
            .unwrap();
        assert_eq!(release.version, Version::new(0, 6, 0));
        assert_eq!(
            release.binary_url,
            "https://example.com/v0.6.0/mcrtc-linux-x86_64"
        );
        assert_eq!(
            release.checksums_url,
            "https://example.com/v0.6.0/SHA256SUMS"
        );
    }

    #[test]
    fn ignores_the_same_or_older_versions() {
        let assets = ["mcrtc-linux-x86_64", CHECKSUMS];
        for tag in ["v0.5.0", "v0.4.9", "0.5.0"] {
            let json = release_json(tag, &assets);
            let found = newer_release(&json, &Version::new(0, 5, 0), "mcrtc-linux-x86_64");
            assert_eq!(found.unwrap(), None, "{tag}");
        }
    }

    #[test]
    fn waits_for_the_binary_and_its_checksums() {
        for assets in [
            &["mcrtc-windows-x86_64.exe", CHECKSUMS][..],
            &["mcrtc-linux-x86_64"],
        ] {
            let json = release_json("v1.0.0", assets);
            let found = newer_release(&json, &Version::new(0, 5, 0), "mcrtc-linux-x86_64");
            assert_eq!(found.unwrap(), None);
        }
    }

    #[test]
    fn rejects_a_tag_that_is_not_a_version() {
        let json = release_json("nightly", &["mcrtc-linux-x86_64", CHECKSUMS]);
        assert!(newer_release(&json, &Version::new(0, 5, 0), "mcrtc-linux-x86_64").is_err());
    }

    #[test]
    fn reads_sha256sum_output() {
        let sums = "ABC123  mcrtc-linux-x86_64\n\
                    def456 *mcrtc-windows-x86_64.exe\n\
                    0789  mcrtc-linux-x86_64.old\n";
        assert_eq!(
            expected_digest(sums, "mcrtc-linux-x86_64").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            expected_digest(sums, "mcrtc-windows-x86_64.exe").as_deref(),
            Some("def456")
        );
        assert_eq!(expected_digest(sums, "mcrtc-macos-arm64"), None);
    }

    #[test]
    fn hashes_as_lowercase_hex() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_crate_version_parses() {
        current();
    }
}
