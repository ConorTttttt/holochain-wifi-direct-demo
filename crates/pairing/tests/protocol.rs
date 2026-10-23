//! The whole pairing exchange over real TCP connections on loopback: one-way
//! QR import, mutual peer registration, single use, rejection and recovery.
//!
//! This exercises the protocol only; no Wi-Fi is involved.

use hcwfd_pairing::invite::{DEFAULT_LIFETIME_S, HostNode};
use hcwfd_pairing::messages::check_hello;
use hcwfd_pairing::protocol::{ExchangeError, host_exchange, join_exchange};
use hcwfd_pairing::session::SessionError;
use hcwfd_pairing::{DirectPeerUrl, EndpointId, HostSession, Invite, PairingError, PeerHello};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const NOW: u64 = 1_761_300_000;

fn endpoint(seed: u8) -> EndpointId {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    EndpointId::from_bytes(key.verifying_key().to_bytes()).unwrap()
}

/// A hello whose single "agent info" is just its direct URL, standing in for
/// a signed record whose URL the caller would decode.
fn hello(name: &str, seed: u8, addr: &str) -> PeerHello {
    let endpoint_id = endpoint(seed);
    let addr = addr.parse().unwrap();
    PeerHello {
        name: name.into(),
        endpoint_id,
        addr,
        agent_infos: vec![DirectPeerUrl { addr, endpoint_id }.to_string()],
    }
}

/// What a real node does with a peer's hello: check it against the
/// connection, then "register" its agent infos.
fn checker(
    observed: IpAddr,
    expected: Option<EndpointId>,
    registered: Arc<Mutex<Vec<String>>>,
) -> impl FnOnce(&PeerHello) -> Result<(), String> {
    move |peer: &PeerHello| {
        let urls: Vec<Option<String>> = peer.agent_infos.iter().cloned().map(Some).collect();
        check_hello(peer, observed, expected.as_ref(), &urls).map_err(|e| e.to_string())?;
        registered
            .lock()
            .unwrap()
            .extend(peer.agent_infos.iter().cloned());
        Ok(())
    }
}

struct Host {
    listener: TcpListener,
    session: HostSession,
    invite: Invite,
    hello: PeerHello,
}

impl Host {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let hello = hello("windows", 1, "127.0.0.1:47001");
        let mut invite = Invite::generate(
            listener.local_addr().unwrap().port(),
            DEFAULT_LIFETIME_S,
            NOW,
        );
        invite.host = Some(HostNode {
            name: hello.name.clone(),
            endpoint_id: hello.endpoint_id,
            addr: hello.addr,
        });
        Self {
            session: HostSession::new(&invite),
            listener,
            invite,
            hello,
        }
    }

    /// Serve one connection on a thread; returns the host state afterwards.
    fn serve_one(
        mut self,
        now: u64,
        registered: Arc<Mutex<Vec<String>>>,
    ) -> thread::JoinHandle<(Self, Result<PeerHello, ExchangeError>)> {
        thread::spawn(move || {
            let (stream, peer) = self.listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let own = self.hello.clone();
            let result = host_exchange(
                stream,
                &mut self.session,
                now,
                &own,
                checker(peer.ip(), None, registered),
            );
            (self, result)
        })
    }
}

fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

/// Ask the host thread to read a connection that is opened and immediately
/// handed to `f`.
fn join(
    invite: &Invite,
    own: &PeerHello,
    registered: Arc<Mutex<Vec<String>>>,
) -> Result<PeerHello, ExchangeError> {
    let expected = invite.host.as_ref().map(|h| h.endpoint_id);
    join_exchange(
        connect(invite.pairing.port),
        invite,
        own,
        checker("127.0.0.1".parse().unwrap(), expected, registered),
    )
}

#[test]
fn one_qr_code_registers_each_peer_with_the_other() {
    let host = Host::new();
    // The joiner only has what the QR code holds.
    let invite = Invite::decode(&host.invite.encode(), NOW).unwrap();
    let host_registered = Arc::new(Mutex::new(Vec::new()));
    let joiner_registered = Arc::new(Mutex::new(Vec::new()));
    let served = host.serve_one(NOW, host_registered.clone());

    let ubuntu = hello("ubuntu", 2, "127.0.0.1:47011");
    let windows = join(&invite, &ubuntu, joiner_registered.clone()).unwrap();
    let (host, result) = served.join().unwrap();

    assert_eq!(result.unwrap(), ubuntu);
    assert_eq!(windows, host.hello);
    assert_eq!(*host_registered.lock().unwrap(), ubuntu.agent_infos);
    assert_eq!(*joiner_registered.lock().unwrap(), host.hello.agent_infos);
    assert_eq!(
        host.session.secret(NOW).err(),
        Some(SessionError::AlreadyUsed)
    );
}

#[test]
fn a_used_invitation_is_refused_without_a_handshake() {
    let host = Host::new();
    let invite = host.invite.clone();
    let served = host.serve_one(NOW, Default::default());
    join(
        &invite,
        &hello("ubuntu", 2, "127.0.0.1:47011"),
        Default::default(),
    )
    .unwrap();
    let (host, _) = served.join().unwrap();

    // A second device with a photo of the same QR code.
    let served = host.serve_one(NOW, Default::default());
    let second = join(
        &invite,
        &hello("intruder", 3, "127.0.0.1:47021"),
        Default::default(),
    );
    let (_, result) = served.join().unwrap();
    assert!(
        matches!(
            result,
            Err(ExchangeError::Session(SessionError::AlreadyUsed))
        ),
        "{result:?}"
    );
    assert!(second.is_err());
}

#[test]
fn an_expired_invitation_is_refused_by_the_host_clock() {
    let host = Host::new();
    let invite = host.invite.clone();
    let late = invite.expires_at + 1;
    let served = host.serve_one(late, Default::default());
    let joined = join(
        &invite,
        &hello("ubuntu", 2, "127.0.0.1:47011"),
        Default::default(),
    );
    let (_, result) = served.join().unwrap();
    assert!(
        matches!(result, Err(ExchangeError::Session(SessionError::Expired))),
        "{result:?}"
    );
    assert!(joined.is_err());
}

#[test]
fn a_wrong_qr_code_fails_and_the_right_one_still_works_after() {
    let host = Host::new();
    let good = host.invite.clone();
    let mut wrong = Invite::generate(good.pairing.port, DEFAULT_LIFETIME_S, NOW);
    wrong.session_id = good.session_id;

    let served = host.serve_one(NOW, Default::default());
    let joined = join(
        &wrong,
        &hello("ubuntu", 2, "127.0.0.1:47011"),
        Default::default(),
    );
    let (host, result) = served.join().unwrap();
    assert!(
        matches!(
            result,
            Err(ExchangeError::Channel(PairingError::AuthenticationFailed))
        ),
        "{result:?}"
    );
    assert!(joined.is_err());
    assert_eq!(host.session.failed_attempts(), 1);

    // Recovery: the genuine joiner pairs on its next attempt.
    let served = host.serve_one(NOW, Default::default());
    join(
        &good,
        &hello("ubuntu", 2, "127.0.0.1:47011"),
        Default::default(),
    )
    .unwrap();
    let (_, result) = served.join().unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn the_host_refuses_a_joiner_bound_to_another_address_and_says_why() {
    let host = Host::new();
    let invite = host.invite.clone();
    let served = host.serve_one(NOW, Default::default());
    // It connects from 127.0.0.1 but claims a node elsewhere.
    let joined = join(
        &invite,
        &hello("ubuntu", 2, "10.9.8.7:47011"),
        Default::default(),
    );
    let (host, result) = served.join().unwrap();
    assert!(
        matches!(result, Err(ExchangeError::WeRejected(_))),
        "{result:?}"
    );
    match joined {
        Err(ExchangeError::PeerRejected(reason)) => {
            assert!(reason.contains("10.9.8.7"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        host.session.is_open(NOW),
        "a refused attempt must not use up the invitation"
    );
}

#[test]
fn the_joiner_refuses_a_host_whose_endpoint_differs_from_the_qr_code() {
    let host = Host::new();
    let mut invite = host.invite.clone();
    invite.host.as_mut().unwrap().endpoint_id = endpoint(9);
    let served = host.serve_one(NOW, Default::default());
    let joined = join(
        &invite,
        &hello("ubuntu", 2, "127.0.0.1:47011"),
        Default::default(),
    );
    let (host, result) = served.join().unwrap();
    assert!(
        matches!(joined, Err(ExchangeError::WeRejected(_))),
        "{joined:?}"
    );
    assert!(
        matches!(result, Err(ExchangeError::PeerRejected(_))),
        "{result:?}"
    );
    assert!(host.session.is_open(NOW));
}

#[test]
fn a_dropped_connection_counts_as_a_failure_but_leaves_the_invitation_usable() {
    let host = Host::new();
    let invite = host.invite.clone();
    let served = host.serve_one(NOW, Default::default());
    drop(connect(invite.pairing.port));
    let (host, result) = served.join().unwrap();
    assert!(
        matches!(result, Err(ExchangeError::Channel(PairingError::Closed))),
        "{result:?}"
    );
    assert!(host.session.is_open(NOW));
}
