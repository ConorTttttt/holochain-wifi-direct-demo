//! The host's record of one invitation: expiry, single use, revocation.

use crate::invite::{Invite, PairingSecret, SessionId};

/// Failed pairing attempts after which the host revokes the invitation.
///
/// A failure is a handshake with the wrong secret or session, a hello that
/// fails its checks, or a dropped connection. Guessing a 256-bit secret is
/// hopeless anyway; the limit stops a device on the group from keeping the
/// pairing service busy.
pub const MAX_FAILED_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    Consumed,
    Revoked,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("the invitation expired; create a new one")]
    Expired,
    #[error("the invitation was already used to pair a peer")]
    AlreadyUsed,
    #[error("the invitation was revoked")]
    Revoked,
}

/// Host-side state of an invitation. The expiry is the host's own clock; the
/// joiner's check is only advisory.
#[derive(Debug)]
pub struct HostSession {
    session_id: SessionId,
    secret: PairingSecret,
    expires_at: u64,
    state: State,
    failed_attempts: u32,
}

impl HostSession {
    pub fn new(invite: &Invite) -> Self {
        Self {
            session_id: invite.session_id,
            secret: invite.pairing.secret.clone(),
            expires_at: invite.expires_at,
            state: State::Open,
            failed_attempts: 0,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// The secret, if the invitation may still be used at `now`.
    pub fn secret(&self, now: u64) -> Result<&PairingSecret, SessionError> {
        match self.state {
            State::Consumed => Err(SessionError::AlreadyUsed),
            State::Revoked => Err(SessionError::Revoked),
            State::Open if now > self.expires_at => Err(SessionError::Expired),
            State::Open => Ok(&self.secret),
        }
    }

    /// Note a failed attempt. Returns true if that revoked the invitation.
    pub fn record_failure(&mut self) -> bool {
        self.failed_attempts += 1;
        if self.state == State::Open && self.failed_attempts >= MAX_FAILED_ATTEMPTS {
            self.state = State::Revoked;
            return true;
        }
        false
    }

    pub fn failed_attempts(&self) -> u32 {
        self.failed_attempts
    }

    /// Mark the invitation used. Only an open, unexpired one can be.
    pub fn consume(&mut self, now: u64) -> Result<(), SessionError> {
        self.secret(now)?;
        self.state = State::Consumed;
        Ok(())
    }

    /// Stop accepting this invitation.
    pub fn revoke(&mut self) {
        if self.state == State::Open {
            self.state = State::Revoked;
        }
    }

    pub fn is_open(&self, now: u64) -> bool {
        self.secret(now).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_761_300_000;

    fn session() -> HostSession {
        HostSession::new(&Invite::generate(47002, 600, NOW))
    }

    #[test]
    fn open_until_it_expires() {
        let session = session();
        assert!(session.secret(NOW).is_ok());
        assert!(session.secret(NOW + 600).is_ok());
        assert_eq!(session.secret(NOW + 601).err(), Some(SessionError::Expired));
    }

    #[test]
    fn can_be_used_only_once() {
        let mut session = session();
        session.consume(NOW).unwrap();
        assert_eq!(session.secret(NOW).err(), Some(SessionError::AlreadyUsed));
        assert_eq!(session.consume(NOW), Err(SessionError::AlreadyUsed));
    }

    #[test]
    fn an_expired_invitation_can_not_be_consumed() {
        let mut session = session();
        assert_eq!(session.consume(NOW + 601), Err(SessionError::Expired));
    }

    #[test]
    fn failures_below_the_limit_leave_it_open() {
        let mut session = session();
        for _ in 1..MAX_FAILED_ATTEMPTS {
            assert!(!session.record_failure());
        }
        assert!(session.is_open(NOW));
        // A good attempt after failed ones still works.
        session.consume(NOW).unwrap();
    }

    #[test]
    fn too_many_failures_revoke_it() {
        let mut session = session();
        for _ in 1..MAX_FAILED_ATTEMPTS {
            session.record_failure();
        }
        assert!(session.record_failure());
        assert_eq!(session.secret(NOW).err(), Some(SessionError::Revoked));
    }

    #[test]
    fn revoking_stops_it_but_does_not_undo_a_use() {
        let mut session = session();
        session.revoke();
        assert_eq!(session.secret(NOW).err(), Some(SessionError::Revoked));

        let mut used = self::session();
        used.consume(NOW).unwrap();
        used.revoke();
        assert_eq!(used.secret(NOW).err(), Some(SessionError::AlreadyUsed));
    }
}
