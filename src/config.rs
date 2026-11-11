//! ICE server and limit configuration, merged from a TOML file and command-line flags.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde::Deserialize;

use crate::edition::Edition;
use crate::invite::normalize_fingerprint;

pub const DEFAULT_STUN: [&str; 2] = [
    "stun:stun.l.google.com:19302",
    "stun:stun.cloudflare.com:3478",
];

/// Options shared by `host` and `join`.
#[derive(Debug, Clone, Default, Args)]
pub struct CommonArgs {
    /// Which Minecraft to tunnel [default: java]
    #[arg(long, value_enum, global = true)]
    pub edition: Option<Edition>,

    /// TOML config file (see README)
    #[arg(long, value_name = "FILE", global = true)]
    pub config: Option<PathBuf>,

    /// STUN server URL, repeatable; replaces the defaults
    #[arg(long, value_name = "URL", global = true)]
    pub stun: Vec<String>,

    /// TURN server URL, e.g. turn:turn.example.com:3478?transport=udp
    #[arg(long, value_name = "URL", global = true)]
    pub turn: Option<String>,

    /// TURN username
    #[arg(long, value_name = "USER", global = true)]
    pub turn_user: Option<String>,

    /// TURN password
    #[arg(long, value_name = "PASS", global = true)]
    pub turn_pass: Option<String>,

    /// Print and accept raw JSON instead of the compressed string
    #[arg(long, global = true)]
    pub raw_json: bool,

    /// Verbose logs (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
}

/// The `mcrtc.toml` schema. Every key is optional and flags win over the file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default)]
    pub stun: Option<Vec<String>>,
    #[serde(default)]
    pub turn: Vec<TurnServer>,
    pub edition: Option<Edition>,
    pub server: Option<String>,
    pub listen: Option<String>,
    pub whitelist_file: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    pub max_peers: Option<usize>,
    pub max_connections: Option<usize>,
    pub world_path: Option<PathBuf>,
    pub sync_path: Option<PathBuf>,
    pub sync_interval_secs: Option<u64>,
    pub rcon_address: Option<String>,
    pub rcon_password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnServer {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl FileConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("could not read config file {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))
    }
}

/// The ICE servers a peer connection will use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IceConfig {
    pub stun: Vec<String>,
    pub turn: Vec<TurnServer>,
}

impl IceConfig {
    pub fn resolve(args: &CommonArgs, file: &FileConfig) -> Result<Self> {
        let stun = if !args.stun.is_empty() {
            args.stun.clone()
        } else if let Some(stun) = &file.stun {
            stun.clone()
        } else {
            DEFAULT_STUN.iter().map(|url| url.to_string()).collect()
        };
        let mut turn = file.turn.clone();
        match (&args.turn, &args.turn_user, &args.turn_pass) {
            (Some(url), Some(username), Some(password)) => turn.push(TurnServer {
                url: url.clone(),
                username: username.clone(),
                password: password.clone(),
            }),
            (None, None, None) => {}
            _ => bail!("--turn, --turn-user and --turn-pass must be used together"),
        }
        let ice = Self { stun, turn };
        ice.validate()?;
        Ok(ice)
    }
}

impl IceConfig {
    pub fn validate(&self) -> Result<()> {
        for url in &self.stun {
            ensure!(
                url.starts_with("stun:") || url.starts_with("stuns:"),
                "`{url}` is not a STUN URL (expected stun:host:port)"
            );
        }
        for server in &self.turn {
            ensure!(
                server.url.starts_with("turn:") || server.url.starts_with("turns:"),
                "`{}` is not a TURN URL (expected turn:host:port)",
                server.url
            );
        }
        Ok(())
    }
}

/// Parses a whitelist file: one DTLS fingerprint per line, `#` starts a comment. Text after the
/// fingerprint is ignored only when it follows a `#`, so `sha-256 AB:CD # alex` works.
pub fn parse_whitelist(text: &str) -> Result<HashSet<String>> {
    let mut ids = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let id = normalize_fingerprint(line);
        let valid = id.split_once(' ').is_some_and(|(algorithm, hash)| {
            !algorithm.is_empty()
                && !hash.is_empty()
                && hash.chars().all(|c| c.is_ascii_hexdigit() || c == ':')
        });
        ensure!(
            valid,
            "line {}: expected an ID like `sha-256 AB:CD:...`, got `{line}`",
            index + 1
        );
        ids.insert(id);
    }
    Ok(ids)
}

pub fn load_whitelist(path: &Path) -> Result<HashSet<String>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("could not read whitelist {}", path.display()))?;
    parse_whitelist(&text).with_context(|| format!("invalid whitelist {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_public_stun() {
        let ice = IceConfig::resolve(&CommonArgs::default(), &FileConfig::default()).unwrap();
        assert_eq!(ice.stun, DEFAULT_STUN);
        assert!(ice.turn.is_empty());
    }

    #[test]
    fn file_overrides_defaults_and_flags_override_file() {
        let file: FileConfig = toml::from_str(r#"stun = ["stun:file.example:3478"]"#).unwrap();
        let ice = IceConfig::resolve(&CommonArgs::default(), &file).unwrap();
        assert_eq!(ice.stun, ["stun:file.example:3478"]);

        let args = CommonArgs {
            stun: vec!["stun:flag.example:3478".into()],
            ..Default::default()
        };
        let ice = IceConfig::resolve(&args, &file).unwrap();
        assert_eq!(ice.stun, ["stun:flag.example:3478"]);
    }

    #[test]
    fn empty_stun_list_in_file_means_no_stun() {
        let file: FileConfig = toml::from_str("stun = []").unwrap();
        assert!(IceConfig::resolve(&CommonArgs::default(), &file)
            .unwrap()
            .stun
            .is_empty());
    }

    #[test]
    fn turn_comes_from_file_and_flags() {
        let file: FileConfig = toml::from_str(
            r#"
            max_peers = 4
            [[turn]]
            url = "turn:file.example:3478"
            username = "u"
            password = "p"
            "#,
        )
        .unwrap();
        assert_eq!(file.max_peers, Some(4));

        let args = CommonArgs {
            turn: Some("turns:flag.example:5349".into()),
            turn_user: Some("a".into()),
            turn_pass: Some("b".into()),
            ..Default::default()
        };
        let ice = IceConfig::resolve(&args, &file).unwrap();
        assert_eq!(ice.turn.len(), 2);
        assert_eq!(ice.turn[1].url, "turns:flag.example:5349");
    }

    #[test]
    fn partial_turn_flags_are_rejected() {
        let args = CommonArgs {
            turn: Some("turn:x:3478".into()),
            ..Default::default()
        };
        assert!(IceConfig::resolve(&args, &FileConfig::default()).is_err());
    }

    #[test]
    fn wrong_url_schemes_are_rejected() {
        let args = CommonArgs {
            stun: vec!["turn:x:3478".into()],
            ..Default::default()
        };
        assert!(IceConfig::resolve(&args, &FileConfig::default()).is_err());
    }

    #[test]
    fn the_edition_comes_from_the_file() {
        let file: FileConfig = toml::from_str(r#"edition = "bedrock""#).unwrap();
        assert_eq!(file.edition, Some(Edition::Bedrock));
        assert!(toml::from_str::<FileConfig>(r#"edition = "pocket""#).is_err());
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        assert!(toml::from_str::<FileConfig>("stunn = []").is_err());
    }

    #[test]
    fn whitelist_parsing() {
        let ids = parse_whitelist("# friends\n\nsha-256 AB:CD # alex\nSHA-256   ef:01\n").unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("sha-256 ab:cd"));
        assert!(ids.contains("sha-256 ef:01"));
    }

    #[test]
    fn whitelist_rejects_garbage_with_the_line_number() {
        let error = parse_whitelist("sha-256 ab\nalex\n").unwrap_err();
        assert!(error.to_string().contains("line 2"), "{error}");
    }
}
