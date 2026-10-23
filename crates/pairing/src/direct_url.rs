//! iroh endpoint IDs and Kitsune2 direct peer URLs.
//!
//! With `irohTransport.directBindAddr` set, kitsune2's iroh transport names a
//! node by `http://<ip>:<port>/direct/<endpoint-id>`: the one socket address
//! its endpoint is bound to, and its 32-byte iroh endpoint ID (an ed25519
//! public key) in lowercase hex. This module parses and checks that form
//! without depending on kitsune2 or iroh.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

/// The path segment kitsune2 uses to mark a direct peer URL.
const DIRECT_PATH: &str = "direct";

/// A full 32-byte iroh endpoint ID, which is an ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointId([u8; 32]);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EndpointIdError {
    #[error("an endpoint ID is 64 hex characters, got {0}")]
    Length(usize),
    #[error("an endpoint ID is hex, and this is not")]
    NotHex,
    #[error("not a valid ed25519 public key")]
    NotAPublicKey,
}

impl EndpointId {
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, EndpointIdError> {
        let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map_err(|_| EndpointIdError::NotAPublicKey)?;
        // Small-order points, such as all zeros, are never a real key.
        if key.is_weak() {
            return Err(EndpointIdError::NotAPublicKey);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first 8 hex characters, for display.
    pub fn short(&self) -> String {
        self.to_string()[..8].to_string()
    }
}

impl FromStr for EndpointId {
    type Err = EndpointIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(EndpointIdError::Length(s.len()));
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(s.get(2 * i..2 * i + 2).ok_or(EndpointIdError::NotHex)?, 16)
                .map_err(|_| EndpointIdError::NotHex)?;
        }
        Self::from_bytes(bytes)
    }
}

impl fmt::Display for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EndpointId({self})")
    }
}

impl Serialize for EndpointId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EndpointId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A Kitsune2 direct peer URL: `http://<ip>:<port>/direct/<endpoint-id>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectPeerUrl {
    pub addr: SocketAddr,
    pub endpoint_id: EndpointId,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DirectUrlError {
    #[error("not a direct peer URL (http://<ip>:<port>/direct/<endpoint-id>): {0}")]
    NotDirect(String),
    #[error("bad endpoint ID in {url}: {source}")]
    EndpointId {
        url: String,
        source: EndpointIdError,
    },
}

impl DirectPeerUrl {
    /// Parse a URL the way kitsune2's iroh transport does: only an IP
    /// literal host, and exactly `/direct/<endpoint-id>` as the path.
    pub fn parse(url: &str) -> Result<Self, DirectUrlError> {
        let not_direct = || DirectUrlError::NotDirect(url.to_string());
        let rest = url.strip_prefix("http://").ok_or_else(not_direct)?;
        let (addr, path) = rest.split_once('/').ok_or_else(not_direct)?;
        let (marker, peer_id) = path.split_once('/').ok_or_else(not_direct)?;
        if marker != DIRECT_PATH || peer_id.contains('/') {
            return Err(not_direct());
        }
        let addr: SocketAddr = addr.parse().map_err(|_| not_direct())?;
        let endpoint_id = peer_id
            .parse()
            .map_err(|source| DirectUrlError::EndpointId {
                url: url.to_string(),
                source,
            })?;
        Ok(Self { addr, endpoint_id })
    }
}

impl fmt::Display for DirectPeerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{}/{DIRECT_PATH}/{}", self.addr, self.endpoint_id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid ed25519 public key, from a fixed test secret key.
    pub(crate) fn test_endpoint_id() -> EndpointId {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        EndpointId::from_bytes(signing.verifying_key().to_bytes()).unwrap()
    }

    #[test]
    fn endpoint_id_round_trips_through_hex() {
        let id = test_endpoint_id();
        let hex = id.to_string();
        assert_eq!(hex.len(), 64);
        assert_eq!(hex, hex.to_lowercase());
        assert_eq!(hex.parse::<EndpointId>().unwrap(), id);
        assert_eq!(hex.to_uppercase().parse::<EndpointId>().unwrap(), id);
    }

    #[test]
    fn endpoint_id_rejects_wrong_length_and_non_hex() {
        let hex = test_endpoint_id().to_string();
        assert_eq!(
            hex[..63].parse::<EndpointId>(),
            Err(EndpointIdError::Length(63))
        );
        assert_eq!(
            format!("{hex}00").parse::<EndpointId>(),
            Err(EndpointIdError::Length(66))
        );
        // A 12-byte BLE advertisement prefix is not a full endpoint ID.
        assert_eq!(
            hex[..24].parse::<EndpointId>(),
            Err(EndpointIdError::Length(24))
        );
        let not_hex = format!("zz{}", &hex[2..]);
        assert_eq!(not_hex.parse::<EndpointId>(), Err(EndpointIdError::NotHex));
        // Multi-byte characters must not panic the slicing.
        let wide = format!("é{}", &hex[2..]);
        assert!(wide.parse::<EndpointId>().is_err());
    }

    #[test]
    fn endpoint_id_rejects_bytes_that_are_not_a_curve_point() {
        // y = 2 is not on the ed25519 curve.
        let mut bytes = [0u8; 32];
        bytes[0] = 2;
        assert_eq!(
            EndpointId::from_bytes(bytes),
            Err(EndpointIdError::NotAPublicKey)
        );
        // All zeros decompresses, but to a small-order point.
        assert_eq!(
            EndpointId::from_bytes([0; 32]),
            Err(EndpointIdError::NotAPublicKey)
        );
    }

    #[test]
    fn direct_url_round_trips_ipv4_and_ipv6() {
        let endpoint_id = test_endpoint_id();
        for addr in ["192.168.137.1:47001", "[fd00::1]:47001"] {
            let url = DirectPeerUrl {
                addr: addr.parse().unwrap(),
                endpoint_id,
            };
            let text = url.to_string();
            assert_eq!(text, format!("http://{addr}/direct/{endpoint_id}"));
            assert_eq!(DirectPeerUrl::parse(&text).unwrap(), url);
        }
    }

    #[test]
    fn urls_that_are_not_direct_are_rejected() {
        let id = test_endpoint_id();
        for url in [
            format!("https://192.168.137.1:47001/direct/{id}"),
            format!("http://relay.example:443/direct/{id}"),
            format!("http://192.168.137.1:3456/{id}"),
            format!("http://192.168.137.1:3456/relay/direct/{id}"),
            format!("http://192.168.137.1/direct/{id}"),
            format!("https://ble.invalid:443/{id}"),
            "http://192.168.137.1:47001/direct/".to_string(),
        ] {
            assert!(DirectPeerUrl::parse(&url).is_err(), "{url}");
        }
    }
}
