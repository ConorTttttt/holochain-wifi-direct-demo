//! The two sides of one pairing exchange, over an already-open stream.
//!
//! Neither side knows about Holochain: each takes a closure that checks the
//! other's [`PeerHello`] and registers its agent infos, and returns the reason
//! to give the peer if it refuses.

use crate::channel::{PairingError, SecureChannel};
use crate::invite::Invite;
use crate::messages::{Message, PeerHello};
use crate::session::{HostSession, SessionError};
use std::io::{Read, Write};

#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Channel(#[from] PairingError),
    #[error("the peer refused to pair: {0}")]
    PeerRejected(String),
    #[error("refused the peer: {0}")]
    WeRejected(String),
    #[error("the peer sent {0} where {1} was expected")]
    Unexpected(&'static str, &'static str),
}

fn kind(message: &Message) -> &'static str {
    match message {
        Message::JoinHello(_) => "join_hello",
        Message::HostHello(_) => "host_hello",
        Message::Rejected { .. } => "rejected",
        Message::Ack => "ack",
    }
}

/// Serve one pairing connection on the host.
///
/// Any failure after the invitation was found usable counts against it (see
/// [`HostSession::record_failure`]). The invitation is consumed only once the
/// joiner has acknowledged the host's hello, so a joiner whose connection
/// drops before then can try again with the same QR code.
pub fn host_exchange<S: Read + Write>(
    stream: S,
    session: &mut HostSession,
    now: u64,
    own: &PeerHello,
    accept: impl FnOnce(&PeerHello) -> Result<(), String>,
) -> Result<PeerHello, ExchangeError> {
    let secret = session.secret(now)?.clone();
    let result = (|| {
        let mut channel = SecureChannel::accept(stream, &secret, session.session_id())?;
        let hello = match channel.recv()? {
            Message::JoinHello(hello) => hello,
            other => return Err(ExchangeError::Unexpected(kind(&other), "join_hello")),
        };
        if let Err(reason) = accept(&hello) {
            let _ = channel.send(&Message::Rejected {
                reason: reason.clone(),
            });
            return Err(ExchangeError::WeRejected(reason));
        }
        channel.send(&Message::HostHello(own.clone()))?;
        match channel.recv()? {
            Message::Ack => Ok(hello),
            Message::Rejected { reason } => Err(ExchangeError::PeerRejected(reason)),
            other => Err(ExchangeError::Unexpected(kind(&other), "ack")),
        }
    })();
    match result {
        Ok(hello) => {
            session.consume(now)?;
            Ok(hello)
        }
        Err(err) => {
            session.record_failure();
            Err(err)
        }
    }
}

/// Pair with the host named by `invite`, over a stream already connected to
/// its pairing port.
pub fn join_exchange<S: Read + Write>(
    stream: S,
    invite: &Invite,
    own: &PeerHello,
    accept: impl FnOnce(&PeerHello) -> Result<(), String>,
) -> Result<PeerHello, ExchangeError> {
    let mut channel = SecureChannel::connect(stream, &invite.pairing.secret, &invite.session_id)?;
    channel.send(&Message::JoinHello(own.clone()))?;
    let hello = match channel.recv()? {
        Message::HostHello(hello) => hello,
        Message::Rejected { reason } => return Err(ExchangeError::PeerRejected(reason)),
        other => return Err(ExchangeError::Unexpected(kind(&other), "host_hello")),
    };
    if let Err(reason) = accept(&hello) {
        let _ = channel.send(&Message::Rejected {
            reason: reason.clone(),
        });
        return Err(ExchangeError::WeRejected(reason));
    }
    channel.send(&Message::Ack)?;
    Ok(hello)
}
