//! QR invitations and the authenticated pairing exchange for Holochain over
//! Wi-Fi Direct.
//!
//! The host (Windows, the Wi-Fi Direct group owner) creates an [`Invite`] and
//! shows it as a QR code. It carries the group's SSID and passphrase, a
//! session ID, a 32-byte single-use pairing secret, an expiry time, and, when
//! they are already known, the host node's iroh endpoint ID and link address.
//!
//! The joiner (Ubuntu) reads the QR code, joins the group, and opens a TCP
//! connection to the host's pairing port. The two run a Noise
//! `NNpsk0_25519_ChaChaPoly_SHA256` handshake keyed by the pairing secret, with
//! the session ID in the prologue, so each side proves it read the same QR
//! code without sending the secret. Over the resulting [`channel::SecureChannel`]
//! they swap [`messages::PeerHello`]s: each node's name, iroh endpoint ID,
//! direct socket address and signed Kitsune2 agent infos. Each side checks the
//! other's hello with [`messages::check_hello`] before registering the agent
//! infos with its conductor. The host's [`session::HostSession`] makes the
//! secret single-use and expiring.
//!
//! Nothing here holds or sends private keys: agent infos are signed public
//! records, and the endpoint ID is a public key.

pub mod channel;
pub mod direct_url;
pub mod invite;
pub mod messages;
pub mod protocol;
pub mod random;
pub mod session;

pub use channel::{PairingError, SecureChannel};
pub use direct_url::{DirectPeerUrl, EndpointId};
pub use invite::{Invite, InviteError};
pub use messages::{Message, PeerHello};
pub use session::{HostSession, SessionError};
