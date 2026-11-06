//! Invite and reply strings: the only signaling mcrtc has.
//!
//! A payload is JSON, deflated, base64url encoded (no padding) and prefixed with `mcrtc1:`, so
//! it stays on one line and survives a trip through Discord or WhatsApp. With `--raw-json` the
//! pretty JSON is used directly.

use std::io::{Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "mcrtc1:";
pub const VERSION: u32 = 1;
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

/// Far above any real SDP. Invites come from other people, so a small deflate bomb must not be
/// able to allocate unbounded memory.
const MAX_DECODED_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadType {
    Offer,
    Answer,
}

impl PayloadType {
    fn noun(self) -> &'static str {
        match self {
            PayloadType::Offer => "invite",
            PayloadType::Answer => "reply",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: PayloadType,
    pub sdp: String,
    pub expires: u64,
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

impl Payload {
    pub fn new(kind: PayloadType, sdp: String, ttl: Duration) -> Self {
        Self {
            v: VERSION,
            kind,
            sdp,
            expires: unix_now() + ttl.as_secs(),
        }
    }

    /// The compact single-line form.
    pub fn encode(&self) -> Result<String> {
        let json = serde_json::to_vec(self)?;
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&json)?;
        let compressed = encoder.finish()?;
        Ok(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(compressed)))
    }

    pub fn encode_raw_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parses either form, whichever was pasted, without judging its contents. Use
    /// [`Payload::validate`] before trusting the result.
    pub fn decode(input: &str) -> Result<Self> {
        // Chat apps and terminals like to wrap long lines.
        let compact: String = input.split_whitespace().collect();
        if compact.is_empty() {
            bail!("nothing was pasted");
        }
        if compact.starts_with('{') {
            return serde_json::from_str(input).context("this is not valid mcrtc JSON");
        }
        let Some(encoded) = compact.strip_prefix(PREFIX) else {
            if let Some((prefix, _)) = compact.split_once(':') {
                if prefix.starts_with("mcrtc") {
                    bail!("unsupported format `{prefix}:`; you and your friend need the same mcrtc version");
                }
            }
            bail!("this does not look like an mcrtc string (it should start with `{PREFIX}`)");
        };

        let compressed = URL_SAFE_NO_PAD
            .decode(encoded)
            .context("the string is corrupted or was cut off while copying (bad base64)")?;
        let mut json = Vec::new();
        DeflateDecoder::new(compressed.as_slice())
            .take(MAX_DECODED_BYTES + 1)
            .read_to_end(&mut json)
            .context("the string is corrupted or was cut off while copying (bad deflate data)")?;
        ensure!(
            json.len() as u64 <= MAX_DECODED_BYTES,
            "the string decodes to more than {MAX_DECODED_BYTES} bytes, refusing it"
        );
        serde_json::from_slice(&json).context("the string does not contain valid mcrtc data")
    }

    /// Checks the version, the direction and the expiry against `now` (unix seconds).
    pub fn validate(&self, expected: PayloadType, now: u64) -> Result<()> {
        ensure!(
            self.v == VERSION,
            "unsupported version {} (this mcrtc speaks version {VERSION}); you and your friend need the same mcrtc version",
            self.v
        );
        ensure!(
            self.kind == expected,
            "expected {} {} but this is {} {}",
            article(expected),
            expected.noun(),
            article(self.kind),
            self.kind.noun()
        );
        ensure!(
            self.expires > now,
            "this {} expired {} seconds ago; ask for a new one",
            self.kind.noun(),
            now - self.expires
        );
        ensure!(
            !self.sdp.trim().is_empty(),
            "the {} has an empty SDP",
            self.kind.noun()
        );
        Ok(())
    }

    /// Decodes and validates a pasted string against the current time.
    pub fn parse(input: &str, expected: PayloadType) -> Result<Self> {
        let payload = Self::decode(input)?;
        payload.validate(expected, unix_now())?;
        Ok(payload)
    }
}

fn article(kind: PayloadType) -> &'static str {
    match kind {
        PayloadType::Offer => "an",
        PayloadType::Answer => "a",
    }
}

/// Every `a=fingerprint` in an SDP, normalized for comparison. The DTLS handshake then proves
/// the peer owns the certificate with this fingerprint, which is what makes it usable as an ID.
pub fn sdp_fingerprints(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter_map(|line| line.trim().strip_prefix("a=fingerprint:"))
        .map(normalize_fingerprint)
        .collect()
}

/// Lowercases `sha-256 AB:CD` to `sha-256 ab:cd` and collapses the separating whitespace.
pub fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: PayloadType) -> Payload {
        Payload {
            v: VERSION,
            kind,
            sdp: "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\na=fingerprint:sha-256 AB:CD\r\n".repeat(20),
            expires: 2_000,
        }
    }

    fn encoded_json(json: &[u8]) -> String {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(json).unwrap();
        let compressed = encoder.finish().unwrap();
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(compressed))
    }

    #[test]
    fn compact_round_trip() {
        let payload = sample(PayloadType::Offer);
        let encoded = payload.encode().unwrap();
        assert!(encoded.starts_with(PREFIX));
        assert!(!encoded.contains(char::is_whitespace));
        assert!(!encoded[PREFIX.len()..].contains(['+', '/', '=']));
        assert_eq!(Payload::decode(&encoded).unwrap(), payload);
    }

    #[test]
    fn raw_json_round_trip() {
        let payload = sample(PayloadType::Answer);
        let json = payload.encode_raw_json().unwrap();
        assert!(json.contains("\"type\": \"answer\""));
        assert_eq!(Payload::decode(&json).unwrap(), payload);
    }

    #[test]
    fn wire_format_field_names_match_the_spec() {
        let payload = sample(PayloadType::Offer);
        let value: serde_json::Value =
            serde_json::from_str(&payload.encode_raw_json().unwrap()).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["type"], "offer");
        assert_eq!(value["expires"], 2_000);
        assert!(value["sdp"].is_string());
    }

    #[test]
    fn whitespace_from_chat_apps_is_ignored() {
        let payload = sample(PayloadType::Offer);
        let encoded = payload.encode().unwrap();
        let (head, tail) = encoded.split_at(encoded.len() / 2);
        let wrapped = format!("  {head}\r\n  {tail}\n");
        assert_eq!(Payload::decode(&wrapped).unwrap(), payload);
    }

    #[test]
    fn accepts_a_payload_before_expiry() {
        sample(PayloadType::Offer)
            .validate(PayloadType::Offer, 1_999)
            .unwrap();
    }

    #[test]
    fn rejects_expired() {
        let payload = sample(PayloadType::Offer);
        for now in [2_000, 2_500] {
            let error = payload.validate(PayloadType::Offer, now).unwrap_err();
            assert!(error.to_string().contains("expired"), "{error}");
        }
    }

    #[test]
    fn new_payload_expires_after_the_ttl() {
        let payload = Payload::new(PayloadType::Offer, "v=0".into(), DEFAULT_TTL);
        let now = unix_now();
        payload.validate(PayloadType::Offer, now).unwrap();
        assert!(payload.validate(PayloadType::Offer, now + 601).is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let mut payload = sample(PayloadType::Offer);
        payload.v = 2;
        let error = payload.validate(PayloadType::Offer, 0).unwrap_err();
        assert!(
            error.to_string().contains("unsupported version 2"),
            "{error}"
        );
    }

    #[test]
    fn rejects_the_wrong_direction() {
        let error = sample(PayloadType::Offer)
            .validate(PayloadType::Answer, 0)
            .unwrap_err();
        assert!(error.to_string().contains("expected a reply"), "{error}");
    }

    #[test]
    fn rejects_empty_sdp() {
        let mut payload = sample(PayloadType::Offer);
        payload.sdp = " ".into();
        assert!(payload.validate(PayloadType::Offer, 0).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        for (input, expected) in [
            ("", "nothing was pasted"),
            ("   \n ", "nothing was pasted"),
            ("hello friend", "does not look like"),
            ("mcrtc2:abcd", "unsupported format"),
            ("mcrtc1:not*base64!", "bad base64"),
            ("mcrtc1:AAAA", "bad deflate"),
            ("{\"v\": 1}", "not valid mcrtc JSON"),
        ] {
            let error = format!("{:#}", Payload::decode(input).unwrap_err());
            assert!(error.contains(expected), "{input:?} gave {error}");
        }
    }

    #[test]
    fn rejects_truncated_strings() {
        let mut encoded = sample(PayloadType::Offer).encode().unwrap();
        encoded.truncate(encoded.len() / 2);
        assert!(Payload::decode(&encoded).is_err());
    }

    #[test]
    fn rejects_valid_deflate_with_wrong_contents() {
        let error = Payload::decode(&encoded_json(b"[1,2,3]")).unwrap_err();
        assert!(error.to_string().contains("valid mcrtc data"), "{error}");
    }

    #[test]
    fn rejects_decompression_bombs() {
        let bomb = vec![b' '; MAX_DECODED_BYTES as usize * 8];
        let encoded = encoded_json(&bomb);
        assert!(
            encoded.len() < 4_096,
            "the test input should be a small string"
        );
        let error = Payload::decode(&encoded).unwrap_err();
        assert!(error.to_string().contains("refusing"), "{error}");
    }

    #[test]
    fn extracts_and_normalizes_fingerprints() {
        let sdp = "v=0\r\na=fingerprint:sha-256 AB:CD:EF\r\nm=application\r\na=fingerprint:sha-256  01:02\r\n";
        assert_eq!(sdp_fingerprints(sdp), ["sha-256 ab:cd:ef", "sha-256 01:02"]);
    }
}
