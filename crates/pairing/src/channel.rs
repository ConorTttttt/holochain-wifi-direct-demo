//! The authenticated pairing channel: a Noise `NNpsk0` handshake keyed by the
//! invitation's pairing secret, over any byte stream.
//!
//! ```text
//! NNpsk0:
//!   -> psk, e        joiner: proves it knows the secret (AEAD tag under it)
//!   <- e, ee         host:   proves it knows the secret too
//! ```
//!
//! The prologue binds the handshake to the protocol and the session ID, so a
//! handshake for another session fails. Each side's ephemeral key makes the
//! session keys fresh: a recorded first message replayed to the host gets a
//! reply the replayer can not use, and nothing it sends after that decrypts.
//! The host therefore commits nothing until it has read the joiner's first
//! transport message.
//!
//! Frames are a 2-byte big-endian length then a Noise message, at most
//! 65535 bytes. The caller sets read and write timeouts on the stream.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{Read, Write};

use crate::invite::{PairingSecret, SessionId};

const NOISE_PARAMS: &str = "Noise_NNpsk0_25519_ChaChaPoly_SHA256";
const PROLOGUE_LABEL: &[u8] = b"holochain-wifi-direct/1 pairing\0";
/// Noise's limit on one message, including its 16-byte tag.
const MAX_FRAME: usize = 65535;
const TAG_LEN: usize = 16;
/// Largest serialized message a [`SecureChannel`] sends or accepts.
pub const MAX_MESSAGE: usize = MAX_FRAME - TAG_LEN;

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error(
        "authentication failed: the other side does not hold the same invitation \
         (wrong QR code, an expired or replaced invitation, or a replayed message)"
    )]
    AuthenticationFailed,
    #[error("the other side closed the pairing connection")]
    Closed,
    #[error("pairing connection: {0}")]
    Io(std::io::Error),
    #[error("pairing protocol error: {0}")]
    Protocol(String),
    #[error("pairing message of {0} bytes is larger than {MAX_MESSAGE}")]
    TooLarge(usize),
}

impl From<std::io::Error> for PairingError {
    fn from(err: std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::Closed
        } else {
            Self::Io(err)
        }
    }
}

fn prologue(session_id: &SessionId) -> Vec<u8> {
    let mut prologue = PROLOGUE_LABEL.to_vec();
    prologue.extend_from_slice(session_id.as_bytes());
    prologue
}

fn builder<'a>(
    secret: &'a PairingSecret,
    prologue: &'a [u8],
) -> Result<snow::Builder<'a>, PairingError> {
    let params = NOISE_PARAMS
        .parse()
        .map_err(|err| PairingError::Protocol(format!("noise parameters: {err}")))?;
    let noise_err = |err: snow::Error| PairingError::Protocol(format!("noise setup: {err}"));
    snow::Builder::new(params)
        .psk(0, secret.as_bytes())
        .map_err(noise_err)?
        .prologue(prologue)
        .map_err(noise_err)
}

fn write_frame<S: Write>(stream: &mut S, frame: &[u8]) -> Result<(), PairingError> {
    let len = u16::try_from(frame.len()).map_err(|_| PairingError::TooLarge(frame.len()))?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(frame)?;
    stream.flush()?;
    Ok(())
}

fn read_frame<S: Read>(stream: &mut S) -> Result<Vec<u8>, PairingError> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    let mut frame = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut frame)?;
    Ok(frame)
}

/// A stream after a successful handshake. Every message is encrypted and
/// authenticated with the session keys, and they arrive in order or not at
/// all: Noise's nonces make a dropped, replayed or reordered frame fail.
pub struct SecureChannel<S> {
    stream: S,
    transport: snow::TransportState,
}

impl<S: Read + Write> SecureChannel<S> {
    /// Run the joiner's side of the handshake.
    pub fn connect(
        mut stream: S,
        secret: &PairingSecret,
        session_id: &SessionId,
    ) -> Result<Self, PairingError> {
        let prologue = prologue(session_id);
        let mut handshake = builder(secret, &prologue)?
            .build_initiator()
            .map_err(|err| PairingError::Protocol(format!("noise: {err}")))?;
        let mut buf = vec![0u8; MAX_FRAME];

        let len = handshake
            .write_message(&[], &mut buf)
            .map_err(|err| PairingError::Protocol(format!("noise: {err}")))?;
        write_frame(&mut stream, &buf[..len])?;

        let reply = read_frame(&mut stream)?;
        handshake
            .read_message(&reply, &mut buf)
            .map_err(|_| PairingError::AuthenticationFailed)?;

        let transport = handshake
            .into_transport_mode()
            .map_err(|err| PairingError::Protocol(format!("noise: {err}")))?;
        Ok(Self { stream, transport })
    }

    /// Run the host's side of the handshake.
    pub fn accept(
        mut stream: S,
        secret: &PairingSecret,
        session_id: &SessionId,
    ) -> Result<Self, PairingError> {
        let prologue = prologue(session_id);
        let mut handshake = builder(secret, &prologue)?
            .build_responder()
            .map_err(|err| PairingError::Protocol(format!("noise: {err}")))?;
        let mut buf = vec![0u8; MAX_FRAME];

        let hello = read_frame(&mut stream)?;
        handshake
            .read_message(&hello, &mut buf)
            .map_err(|_| PairingError::AuthenticationFailed)?;

        let len = handshake
            .write_message(&[], &mut buf)
            .map_err(|err| PairingError::Protocol(format!("noise: {err}")))?;
        write_frame(&mut stream, &buf[..len])?;

        let transport = handshake
            .into_transport_mode()
            .map_err(|err| PairingError::Protocol(format!("noise: {err}")))?;
        Ok(Self { stream, transport })
    }

    pub fn send<T: Serialize>(&mut self, message: &T) -> Result<(), PairingError> {
        let plain = serde_json::to_vec(message)
            .map_err(|err| PairingError::Protocol(format!("encoding a message: {err}")))?;
        if plain.len() > MAX_MESSAGE {
            return Err(PairingError::TooLarge(plain.len()));
        }
        let mut buf = vec![0u8; plain.len() + TAG_LEN];
        let len = self
            .transport
            .write_message(&plain, &mut buf)
            .map_err(|err| PairingError::Protocol(format!("noise: {err}")))?;
        write_frame(&mut self.stream, &buf[..len])
    }

    pub fn recv<T: DeserializeOwned>(&mut self) -> Result<T, PairingError> {
        let frame = read_frame(&mut self.stream)?;
        let mut buf = vec![0u8; frame.len()];
        let len = self
            .transport
            .read_message(&frame, &mut buf)
            .map_err(|_| PairingError::AuthenticationFailed)?;
        serde_json::from_slice(&buf[..len])
            .map_err(|err| PairingError::Protocol(format!("unreadable message: {err}")))
    }

    pub fn get_ref(&self) -> &S {
        &self.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        for s in [&client, &server] {
            s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
        }
        (client, server)
    }

    /// A stream that records what is written to it, for replaying later.
    struct Recorder<S> {
        inner: S,
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl<S: Read> Read for Recorder<S> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl<S: Write> Write for Recorder<S> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = self.inner.write(buf)?;
            self.written.lock().unwrap().extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    #[test]
    fn same_secret_and_session_exchange_messages_both_ways() {
        let (client, server) = pair();
        let secret = PairingSecret::generate();
        let session = SessionId::generate();
        let (host_secret, host_session) = (secret.clone(), session);
        let host = thread::spawn(move || {
            let mut channel = SecureChannel::accept(server, &host_secret, &host_session).unwrap();
            let got: String = channel.recv().unwrap();
            channel.send(&format!("{got} back")).unwrap();
        });
        let mut channel = SecureChannel::connect(client, &secret, &session).unwrap();
        channel.send(&"hello".to_string()).unwrap();
        assert_eq!(channel.recv::<String>().unwrap(), "hello back");
        host.join().unwrap();
    }

    #[test]
    fn a_wrong_secret_fails_on_both_sides() {
        let (client, server) = pair();
        let session = SessionId::generate();
        let host = thread::spawn(move || {
            SecureChannel::accept(server, &PairingSecret::generate(), &session).err()
        });
        let joiner = SecureChannel::connect(client, &PairingSecret::generate(), &session);
        let host_err = host.join().unwrap();
        assert!(
            matches!(host_err, Some(PairingError::AuthenticationFailed)),
            "{host_err:?}"
        );
        // The host hung up without replying.
        assert!(matches!(
            joiner.err(),
            Some(PairingError::Closed | PairingError::Io(_))
        ));
    }

    #[test]
    fn a_different_session_fails_even_with_the_same_secret() {
        let (client, server) = pair();
        let secret = PairingSecret::generate();
        let host_secret = secret.clone();
        let host = thread::spawn(move || {
            SecureChannel::accept(server, &host_secret, &SessionId::generate()).err()
        });
        let _ = SecureChannel::connect(client, &secret, &SessionId::generate());
        assert!(matches!(
            host.join().unwrap(),
            Some(PairingError::AuthenticationFailed)
        ));
    }

    #[test]
    fn an_impostor_host_is_detected_by_the_joiner() {
        // A pairing service that does not know the secret can only answer
        // with bytes the joiner rejects.
        let (client, mut server) = pair();
        let session = SessionId::generate();
        let impostor = thread::spawn(move || {
            let _ = read_frame(&mut server);
            // Any 48 bytes: an ephemeral key and a tag the joiner must reject.
            write_frame(&mut server, &[0x42; 48]).unwrap();
        });
        let result = SecureChannel::connect(client, &PairingSecret::generate(), &session);
        assert!(matches!(
            result.err(),
            Some(PairingError::AuthenticationFailed)
        ));
        impostor.join().unwrap();
    }

    #[test]
    fn replayed_messages_do_not_open_a_new_session() {
        let secret = PairingSecret::generate();
        let session = SessionId::generate();

        // A genuine session, with everything the joiner sent recorded.
        let (client, server) = pair();
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = Recorder {
            inner: client,
            written: written.clone(),
        };
        let (host_secret, host_session) = (secret.clone(), session);
        let host = thread::spawn(move || {
            let mut channel = SecureChannel::accept(server, &host_secret, &host_session).unwrap();
            channel.recv::<String>().unwrap()
        });
        let mut channel = SecureChannel::connect(recorder, &secret, &session).unwrap();
        channel.send(&"join me".to_string()).unwrap();
        assert_eq!(host.join().unwrap(), "join me");
        let recorded = written.lock().unwrap().clone();

        // Replay the joiner's bytes to a fresh host session.
        let (mut attacker, server) = pair();
        let host = thread::spawn(move || {
            let mut channel = SecureChannel::accept(server, &secret, &session)?;
            channel.recv::<String>()
        });
        attacker.write_all(&recorded).unwrap();
        let result = host.join().unwrap();
        assert!(
            matches!(result, Err(PairingError::AuthenticationFailed)),
            "a replayed message was accepted: {result:?}"
        );
    }

    #[test]
    fn oversized_messages_are_refused_before_sending() {
        let (client, server) = pair();
        let secret = PairingSecret::generate();
        let session = SessionId::generate();
        let host_secret = secret.clone();
        let host = thread::spawn(move || {
            let _channel = SecureChannel::accept(server, &host_secret, &session).unwrap();
        });
        let mut channel = SecureChannel::connect(client, &secret, &session).unwrap();
        let big = "x".repeat(MAX_MESSAGE);
        assert!(matches!(channel.send(&big), Err(PairingError::TooLarge(_))));
        host.join().unwrap();
    }
}
