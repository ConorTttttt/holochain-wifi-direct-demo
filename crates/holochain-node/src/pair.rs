//! The pairing service on the host, and the pairing client on the joiner,
//! connecting [`hcwfd_pairing::protocol`] to TCP and to the conductor.
//!
//! The host listens on its Wi-Fi Direct address only, for one peer, and only
//! while the invitation is open. Both sides check the other's hello and let
//! their conductor verify the agent infos' signatures before registering
//! them, so nothing reaches the peer store unchecked.

use crate::node::Node;
use crate::unix_now;
use anyhow::{Context, Result, bail};
use hcwfd_pairing::messages::check_hello;
use hcwfd_pairing::protocol::{ExchangeError, host_exchange, join_exchange};
use hcwfd_pairing::{HostSession, Invite, PairingError, PeerHello};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Read and write timeout on a pairing connection.
const IO_TIMEOUT: Duration = Duration::from_secs(20);

/// Check a peer's hello and register its agent infos with our conductor.
fn accept_peer(
    node: &Node,
    runtime: &tokio::runtime::Handle,
    hello: &PeerHello,
    observed: IpAddr,
    expected_endpoint: Option<&hcwfd_pairing::EndpointId>,
) -> Result<(), String> {
    if hello.name == node.name {
        return Err(format!(
            "both nodes are named '{}'; start one of them with a different --name",
            node.name
        ));
    }
    let urls = node.agent_info_urls(&hello.agent_infos)?;
    check_hello(hello, observed, expected_endpoint, &urls).map_err(|err| err.to_string())?;
    runtime
        .block_on(node.add_agent_infos(hello.agent_infos.clone()))
        .map_err(|err| format!("{err:#}"))
}

pub struct PairedPeer {
    pub hello: PeerHello,
    pub from: SocketAddr,
}

/// Serve the pairing port until one peer pairs, or the invitation expires,
/// is revoked by too many failed attempts, or `stop` is set.
pub async fn serve(
    listen: SocketAddr,
    mut session: HostSession,
    own: PeerHello,
    node: Node,
    stop: Arc<AtomicBool>,
) -> Result<PairedPeer> {
    let runtime = tokio::runtime::Handle::current();
    let listener =
        TcpListener::bind(listen).with_context(|| format!("listening for pairing on {listen}"))?;
    listener.set_nonblocking(true)?;
    tokio::task::spawn_blocking(move || {
        loop {
            if stop.load(Ordering::Relaxed) {
                session.revoke();
                bail!("pairing stopped; the invitation is revoked");
            }
            let now = unix_now();
            if let Err(err) = session.secret(now) {
                bail!("{err}");
            }
            let (stream, from) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                Err(err) => return Err(err).context("accepting a pairing connection"),
            };
            println!("pairing   connection from {from}");
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(IO_TIMEOUT))?;
            stream.set_write_timeout(Some(IO_TIMEOUT))?;
            let result = host_exchange(stream, &mut session, now, &own, |hello| {
                accept_peer(&node, &runtime, hello, from.ip(), None)
            });
            match result {
                Ok(hello) => return Ok(PairedPeer { hello, from }),
                Err(err) => {
                    println!(
                        "pairing   attempt from {from} failed: {err} ({} failed so far)",
                        session.failed_attempts()
                    );
                    if let Err(closed) = session.secret(unix_now()) {
                        bail!("{closed}");
                    }
                }
            }
        }
    })
    .await
    .context("the pairing service stopped")?
}

/// Connect to the host's pairing service from `local_ip` and pair, retrying
/// while the service is not up yet or the connection drops, until `timeout`.
pub async fn join(
    host: SocketAddr,
    local_ip: IpAddr,
    invite: Invite,
    own: PeerHello,
    node: Node,
    timeout: Duration,
) -> Result<PeerHello> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + timeout;
        let expected = invite.host.as_ref().map(|h| h.endpoint_id);
        let mut announced = false;
        loop {
            let attempt = connect_from(local_ip, host).and_then(|stream| {
                join_exchange(stream, &invite, &own, |hello| {
                    accept_peer(&node, &runtime, hello, host.ip(), expected.as_ref())
                })
                .map_err(anyhow::Error::from)
            });
            let err = match attempt {
                Ok(hello) => return Ok(hello),
                Err(err) => err,
            };
            // Retry only what may be temporary: the service not listening
            // yet, or a dropped connection. A refusal is final.
            let retryable = match err.downcast_ref::<ExchangeError>() {
                Some(ExchangeError::Channel(PairingError::Closed | PairingError::Io(_))) => true,
                Some(_) => false,
                None => true,
            };
            if !retryable || Instant::now() >= deadline {
                return Err(err.context(format!("pairing with {host}")));
            }
            if !announced {
                println!("pairing   {host} is not answering yet ({err:#}); retrying");
                announced = true;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    })
    .await
    .context("the pairing client stopped")?
}

/// A TCP connection to `host` whose source is `local_ip`, so it leaves over
/// the same link the Holochain endpoint is bound to.
fn connect_from(local_ip: IpAddr, host: SocketAddr) -> Result<TcpStream> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(host), Type::STREAM, Some(Protocol::TCP))?;
    socket
        .bind(&SocketAddr::new(local_ip, 0).into())
        .with_context(|| format!("binding the pairing connection to {local_ip}"))?;
    socket
        .connect_timeout(&host.into(), Duration::from_secs(5))
        .with_context(|| format!("connecting to the pairing service at {host}"))?;
    let stream: TcpStream = socket.into();
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}
