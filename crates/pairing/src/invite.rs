//! The invitation a host shows as a QR code.
//!
//! The QR code holds `HCWFD1:` followed by the invitation as unpadded
//! base64url JSON, so it survives being pasted through a terminal, a chat or
//! a text file as one token. Version 1's JSON:
//!
//! ```json
//! {
//!   "protocol": "holochain-wifi-direct",
//!   "version": 1,
//!   "session_id": "<16 random bytes, base64url>",
//!   "expires_at": 1761300000,
//!   "wifi": { "mode": "wifi-direct-legacy", "ssid": "DIRECT-hc-…", "passphrase": "…" },
//!   "pairing": { "secret": "<32 random bytes, base64url>", "port": 47002 },
//!   "host": { "name": "windows", "endpoint_id": "<64 hex>", "addr": "192.168.137.1:47001" }
//! }
//! ```
//!
//! `host` is present only when the host's conductor was already bound to its
//! Wi-Fi Direct address when the QR code was made. Windows may give the group
//! owner adapter its address only once the first client associates, so the
//! joiner never depends on it: it finds the host at the DHCP gateway of the
//! group it joined, and learns the host's endpoint ID over the authenticated
//! pairing channel. When `host` is present, the joiner also checks that the
//! endpoint ID it is given matches.
//!
//! The SSID, passphrase and pairing secret are short-lived credentials. The
//! [`fmt::Debug`] output of every type here redacts them, so they never reach
//! a log by accident.

use crate::direct_url::EndpointId;
use crate::random;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::net::SocketAddr;
use zeroize::Zeroize;

pub const PROTOCOL: &str = "holochain-wifi-direct";
pub const VERSION: u32 = 1;
/// Prefix of the QR code text; the digit is the major format version.
pub const PREFIX: &str = "HCWFD1:";
/// The only Wi-Fi mode of version 1: Ubuntu joins Windows' Wi-Fi Direct group
/// as a legacy (WPA2-PSK) client.
pub const WIFI_MODE_LEGACY: &str = "wifi-direct-legacy";
/// Longest QR text accepted. A real invitation is about 450 characters; this
/// bounds the work done on hostile input.
pub const MAX_TEXT_LEN: usize = 2048;
/// How far the joiner's clock may be behind or ahead of the host's. The host
/// enforces the expiry by its own clock regardless.
pub const CLOCK_SKEW_S: u64 = 5 * 60;
/// Longest lifetime an invitation may claim.
pub const MAX_LIFETIME_S: u64 = 24 * 60 * 60;

/// A 32-byte single-use secret, the Noise PSK of the pairing channel.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingSecret([u8; 32]);

impl Drop for PairingSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl PairingSecret {
    pub fn generate() -> Self {
        Self(random::bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingSecret(<redacted>)")
    }
}

/// A random 16-byte session ID. Not secret: it names the session, and binds
/// the pairing handshake to it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId([u8; 16]);

impl SessionId {
    pub fn generate() -> Self {
        Self(random::bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The first 8 characters, for firewall rule and profile names.
    pub fn short(&self) -> String {
        self.to_string()[..8].to_string()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({self})")
    }
}

fn decode_fixed<const N: usize, E: serde::de::Error>(s: &str, what: &str) -> Result<[u8; N], E> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| E::custom(format!("{what} is not base64url")))?;
    bytes
        .try_into()
        .map_err(|_| E::custom(format!("{what} must be {N} bytes")))
}

impl Serialize for SessionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self(decode_fixed(&s, "session_id")?))
    }
}

impl Serialize for PairingSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for PairingSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut s = String::deserialize(deserializer)?;
        let secret = decode_fixed(&s, "pairing secret");
        s.zeroize();
        Ok(Self(secret?))
    }
}

/// The Wi-Fi Direct group to join.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiCredentials {
    pub mode: String,
    pub ssid: String,
    pub passphrase: String,
}

impl fmt::Debug for WifiCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WifiCredentials")
            .field("mode", &self.mode)
            .field("ssid", &self.ssid)
            .field("passphrase", &"<redacted>")
            .finish()
    }
}

impl Drop for WifiCredentials {
    fn drop(&mut self) {
        self.passphrase.zeroize();
    }
}

/// How to reach and authenticate the host's pairing service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingInfo {
    pub secret: PairingSecret,
    /// TCP port of the pairing service, on the host's Wi-Fi Direct address.
    pub port: u16,
}

/// The host node, if it was already running when the QR code was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostNode {
    pub name: String,
    pub endpoint_id: EndpointId,
    /// The host conductor's direct socket address on the Wi-Fi Direct link.
    pub addr: SocketAddr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invite {
    pub protocol: String,
    pub version: u32,
    pub session_id: SessionId,
    /// UNIX time, in seconds, after which the host refuses to pair.
    pub expires_at: u64,
    pub wifi: WifiCredentials,
    pub pairing: PairingInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostNode>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InviteError {
    #[error("this is not a Holochain Wi-Fi Direct invitation (it does not start with {PREFIX})")]
    NotAnInvite,
    #[error("the invitation is {0} characters, more than the {MAX_TEXT_LEN} allowed")]
    TooLarge(usize),
    #[error("the invitation is damaged: {0}")]
    Malformed(String),
    #[error("the invitation is for protocol '{0}', not {PROTOCOL}")]
    WrongProtocol(String),
    #[error(
        "invitation format version {0} is not supported; this program reads version {VERSION}. Update both machines"
    )]
    UnsupportedVersion(u64),
    #[error("the invitation expired {0}s ago; create a new one on the host")]
    Expired(u64),
    #[error("the invitation is invalid: {0}")]
    Invalid(String),
}

/// Lifetime of a new invitation unless the host asks for another.
pub const DEFAULT_LIFETIME_S: u64 = 10 * 60;

impl Invite {
    /// A new invitation with fresh random Wi-Fi credentials, session ID and
    /// pairing secret.
    pub fn generate(pairing_port: u16, lifetime_s: u64, now: u64) -> Self {
        Self {
            protocol: PROTOCOL.to_string(),
            version: VERSION,
            session_id: SessionId::generate(),
            expires_at: now + lifetime_s,
            wifi: WifiCredentials {
                mode: WIFI_MODE_LEGACY.to_string(),
                // Wi-Fi Direct group SSIDs start with DIRECT-.
                ssid: format!("DIRECT-hc-{}", random::string(random::ALPHANUMERIC, 6)),
                // About 143 bits.
                passphrase: random::string(random::ALPHANUMERIC, 24),
            },
            pairing: PairingInfo {
                secret: PairingSecret::generate(),
                port: pairing_port,
            },
            host: None,
        }
    }

    /// The QR code text.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("an invitation always serializes");
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Parse and check QR code text, as read from a camera, an image or a
    /// paste. `now` is UNIX time in seconds.
    pub fn decode(text: &str, now: u64) -> Result<Self, InviteError> {
        let text = text.trim();
        if text.len() > MAX_TEXT_LEN {
            return Err(InviteError::TooLarge(text.len()));
        }
        let Some(body) = strip_prefix_ignore_case(text, PREFIX) else {
            // A newer major version announces itself in the prefix.
            if let Some(rest) = strip_prefix_ignore_case(text, "HCWFD")
                && let Some((version, _)) = rest.split_once(':')
                && let Ok(version) = version.parse::<u64>()
            {
                return Err(InviteError::UnsupportedVersion(version));
            }
            return Err(InviteError::NotAnInvite);
        };
        let json = URL_SAFE_NO_PAD
            .decode(body.trim_end_matches('='))
            .map_err(|_| InviteError::Malformed("not base64url".into()))?;

        // Look at the protocol and version before the rest, so that a newer
        // format gets a clear message instead of a parse error.
        let value: serde_json::Value = serde_json::from_slice(&json)
            .map_err(|err| InviteError::Malformed(format!("not JSON: {err}")))?;
        let protocol = value
            .get("protocol")
            .and_then(|p| p.as_str())
            .ok_or_else(|| InviteError::Malformed("no protocol".into()))?;
        if protocol != PROTOCOL {
            return Err(InviteError::WrongProtocol(
                protocol.chars().take(64).collect(),
            ));
        }
        let version = value
            .get("version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| InviteError::Malformed("no version".into()))?;
        if version != u64::from(VERSION) {
            return Err(InviteError::UnsupportedVersion(version));
        }

        let invite: Invite =
            serde_json::from_value(value).map_err(|err| InviteError::Malformed(err.to_string()))?;
        invite.check(now)?;
        Ok(invite)
    }

    /// Check every field, and the expiry with [`CLOCK_SKEW_S`] of slack.
    pub fn check(&self, now: u64) -> Result<(), InviteError> {
        let invalid = |msg: &str| Err(InviteError::Invalid(msg.to_string()));
        if self.wifi.mode != WIFI_MODE_LEGACY {
            return Err(InviteError::Invalid(format!(
                "Wi-Fi mode '{}' is not supported",
                self.wifi.mode.chars().take(32).collect::<String>()
            )));
        }
        if self.wifi.ssid.is_empty() || self.wifi.ssid.len() > 32 {
            return invalid("the SSID must be 1 to 32 bytes");
        }
        if self.wifi.ssid.chars().any(char::is_control) {
            return invalid("the SSID contains control characters");
        }
        let pass = &self.wifi.passphrase;
        if !(8..=63).contains(&pass.len()) || !pass.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
            return invalid("the Wi-Fi passphrase must be 8 to 63 printable ASCII characters");
        }
        if self.pairing.port == 0 {
            return invalid("the pairing port is 0");
        }
        if let Some(host) = &self.host {
            check_name(&host.name).map_err(InviteError::Invalid)?;
            if host.addr.port() == 0 || host.addr.ip().is_unspecified() {
                return invalid("the host address must be a specific IP address and port");
            }
        }
        if now > self.expires_at + CLOCK_SKEW_S {
            return Err(InviteError::Expired(now - self.expires_at));
        }
        if self.expires_at > now + MAX_LIFETIME_S + CLOCK_SKEW_S {
            return invalid("the expiry is more than a day away");
        }
        Ok(())
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    // QR scanners in alphanumeric mode may upper-case the prefix; the
    // base64url body is case-sensitive and is left alone.
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// A node name: 1 to 32 letters, digits, `-`, `_` or `.`.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 {
        return Err("a node name must be 1 to 32 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "node name '{}' may only contain letters, digits, '-', '_' and '.'",
            name.escape_default()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direct_url::tests::test_endpoint_id;

    const NOW: u64 = 1_761_300_000;

    fn invite() -> Invite {
        let mut invite = Invite::generate(47002, DEFAULT_LIFETIME_S, NOW);
        invite.host = Some(HostNode {
            name: "windows".into(),
            endpoint_id: test_endpoint_id(),
            addr: "192.168.137.1:47001".parse().unwrap(),
        });
        invite
    }

    fn encode_json(value: serde_json::Value) -> String {
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(value.to_string()))
    }

    #[test]
    fn round_trips_through_qr_text() {
        let invite = invite();
        let text = invite.encode();
        assert!(text.starts_with(PREFIX));
        assert!(text.len() < 600, "{} characters", text.len());
        assert_eq!(Invite::decode(&text, NOW).unwrap(), invite);
    }

    #[test]
    fn round_trips_without_a_host_node() {
        let invite = Invite::generate(47002, DEFAULT_LIFETIME_S, NOW);
        let text = invite.encode();
        assert!(
            !URL_SAFE_NO_PAD
                .decode(&text[PREFIX.len()..])
                .unwrap()
                .is_empty()
        );
        assert_eq!(Invite::decode(&text, NOW).unwrap(), invite);
    }

    #[test]
    fn accepts_surrounding_whitespace_and_an_upper_case_prefix() {
        let invite = invite();
        let text = invite.encode();
        let shouted = format!("  {}{}\n", PREFIX.to_lowercase(), &text[PREFIX.len()..]);
        assert_eq!(Invite::decode(&shouted, NOW).unwrap(), invite);
    }

    #[test]
    fn generated_credentials_are_fresh_and_well_formed() {
        let a = Invite::generate(47002, 600, NOW);
        let b = Invite::generate(47002, 600, NOW);
        assert_ne!(a.session_id, b.session_id);
        assert_ne!(a.pairing.secret, b.pairing.secret);
        assert_ne!(a.wifi.passphrase, b.wifi.passphrase);
        assert_ne!(a.wifi.ssid, b.wifi.ssid);
        assert!(a.wifi.ssid.starts_with("DIRECT-"));
        assert_eq!(a.wifi.passphrase.len(), 24);
        a.check(NOW).unwrap();
    }

    #[test]
    fn debug_output_redacts_every_secret() {
        let invite = invite();
        let debug = format!("{invite:?}");
        assert!(!debug.contains(&invite.wifi.passphrase), "{debug}");
        assert!(
            !debug.contains(&URL_SAFE_NO_PAD.encode(invite.pairing.secret.as_bytes())),
            "{debug}"
        );
        assert!(debug.contains("redacted"));
        assert!(debug.contains(&invite.wifi.ssid));
    }

    #[test]
    fn rejects_text_that_is_not_an_invite() {
        for text in [
            "",
            "hello",
            "WIFI:S:DIRECT-x;T:WPA;P:pass;;",
            "https://example.com",
        ] {
            assert_eq!(
                Invite::decode(text, NOW),
                Err(InviteError::NotAnInvite),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_oversized_text_before_decoding_it() {
        let text = format!("{PREFIX}{}", "A".repeat(MAX_TEXT_LEN));
        assert_eq!(
            Invite::decode(&text, NOW),
            Err(InviteError::TooLarge(MAX_TEXT_LEN + PREFIX.len()))
        );
    }

    #[test]
    fn rejects_malformed_bodies() {
        for body in ["!!!", "bm90IGpzb24", "e30"] {
            let result = Invite::decode(&format!("{PREFIX}{body}"), NOW);
            assert!(
                matches!(result, Err(InviteError::Malformed(_))),
                "{body}: {result:?}"
            );
        }
        // A truncated QR read.
        let text = invite().encode();
        let result = Invite::decode(&text[..text.len() / 2], NOW);
        assert!(
            matches!(result, Err(InviteError::Malformed(_))),
            "{result:?}"
        );
    }

    #[test]
    fn reports_newer_format_versions_clearly() {
        assert_eq!(
            Invite::decode("HCWFD2:e30", NOW),
            Err(InviteError::UnsupportedVersion(2))
        );
        let mut value = serde_json::to_value(invite()).unwrap();
        value["version"] = 2.into();
        assert_eq!(
            Invite::decode(&encode_json(value), NOW),
            Err(InviteError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn rejects_other_protocols_and_unknown_fields() {
        let mut value = serde_json::to_value(invite()).unwrap();
        value["protocol"] = "something-else".into();
        assert_eq!(
            Invite::decode(&encode_json(value), NOW),
            Err(InviteError::WrongProtocol("something-else".into()))
        );

        let mut value = serde_json::to_value(invite()).unwrap();
        value["wifi"]["extra"] = "x".into();
        assert!(matches!(
            Invite::decode(&encode_json(value), NOW),
            Err(InviteError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_expired_invitations_beyond_the_clock_skew() {
        let invite = invite();
        let text = invite.encode();
        let expires_at = invite.expires_at;
        assert!(Invite::decode(&text, expires_at).is_ok());
        assert!(Invite::decode(&text, expires_at + CLOCK_SKEW_S).is_ok());
        assert_eq!(
            Invite::decode(&text, expires_at + CLOCK_SKEW_S + 1),
            Err(InviteError::Expired(CLOCK_SKEW_S + 1))
        );
    }

    #[test]
    fn rejects_lifetimes_longer_than_a_day() {
        let invite = Invite::generate(47002, MAX_LIFETIME_S + CLOCK_SKEW_S + 1, NOW);
        assert!(matches!(
            Invite::decode(&invite.encode(), NOW),
            Err(InviteError::Invalid(_))
        ));
    }

    type Mutation = Box<dyn Fn(&mut serde_json::Value)>;

    #[test]
    fn rejects_bad_fields() {
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "short passphrase",
                Box::new(|v| v["wifi"]["passphrase"] = "short".into()),
            ),
            (
                "long passphrase",
                Box::new(|v| v["wifi"]["passphrase"] = "x".repeat(64).into()),
            ),
            ("empty ssid", Box::new(|v| v["wifi"]["ssid"] = "".into())),
            (
                "long ssid",
                Box::new(|v| v["wifi"]["ssid"] = "x".repeat(33).into()),
            ),
            (
                "other mode",
                Box::new(|v| v["wifi"]["mode"] = "hotspot".into()),
            ),
            ("port 0", Box::new(|v| v["pairing"]["port"] = 0.into())),
            (
                "short secret",
                Box::new(|v| v["pairing"]["secret"] = "AAAA".into()),
            ),
            (
                "short session",
                Box::new(|v| v["session_id"] = "AAAA".into()),
            ),
            (
                "bad endpoint",
                Box::new(|v| v["host"]["endpoint_id"] = "00".repeat(32).into()),
            ),
            ("bad name", Box::new(|v| v["host"]["name"] = "a b".into())),
            (
                "unspecified addr",
                Box::new(|v| v["host"]["addr"] = "0.0.0.0:47001".into()),
            ),
        ];
        for (what, mutate) in cases {
            let mut value = serde_json::to_value(invite()).unwrap();
            mutate(&mut value);
            assert!(
                matches!(
                    Invite::decode(&encode_json(value), NOW),
                    Err(InviteError::Invalid(_) | InviteError::Malformed(_))
                ),
                "{what}"
            );
        }
    }

    #[test]
    fn node_names_are_restricted() {
        assert!(check_name("ubuntu").is_ok());
        assert!(check_name("hp-pro_mini.400").is_ok());
        for bad in ["", "a b", "x".repeat(33).as_str(), "naïve", "a\nb"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }
}
