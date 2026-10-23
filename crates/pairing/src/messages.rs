//! What the two nodes say over the pairing channel, and the checks on it.
//!
//! ```text
//! joiner                                   host
//!   JoinHello { name, endpoint, addr, agent infos } ->
//!                                          checks it, registers the infos
//!   <- HostHello { name, endpoint, addr, agent infos }   or Rejected { reason }
//! checks it, registers the infos
//!   Ack ->                                 marks the invitation used
//! ```

use crate::direct_url::{DirectPeerUrl, EndpointId};
use crate::invite::check_name;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};

/// Most agent infos a hello may carry. A node here runs one agent.
pub const MAX_AGENT_INFOS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    JoinHello(PeerHello),
    HostHello(PeerHello),
    Rejected { reason: String },
    Ack,
}

/// A node's public peer information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerHello {
    /// The node's name, shown as "Peer Connected: <name>".
    pub name: String,
    /// The full iroh endpoint ID of the node's kitsune2 endpoint.
    pub endpoint_id: EndpointId,
    /// The socket address that endpoint is bound to on the link.
    pub addr: SocketAddr,
    /// Signed Kitsune2 agent infos, encoded as `AgentInfoSigned::encode`
    /// does. Their signatures are checked by the conductor that adds them.
    pub agent_infos: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HelloError {
    #[error("bad peer name: {0}")]
    Name(String),
    #[error("the peer sent no agent infos")]
    NoAgentInfos,
    #[error("the peer sent {0} agent infos, more than {MAX_AGENT_INFOS}")]
    TooManyAgentInfos(usize),
    #[error(
        "the peer says its node is at {claimed}, but its pairing connection came from {observed}"
    )]
    AddressMismatch { claimed: IpAddr, observed: IpAddr },
    #[error("the invitation names host endpoint {expected}, but the peer is endpoint {actual}")]
    EndpointMismatch {
        expected: EndpointId,
        actual: EndpointId,
    },
    #[error("an agent info has no peer URL")]
    NoUrl,
    #[error("an agent info's URL {0} is not a direct peer URL")]
    NotDirect(String),
    #[error("an agent info's URL {url} does not match the peer's endpoint {endpoint_id} at {addr}")]
    UrlMismatch {
        url: String,
        endpoint_id: EndpointId,
        addr: SocketAddr,
    },
}

/// Check a peer's hello before registering its agent infos.
///
/// - `observed_ip` is the source address of the pairing connection. The
///   peer's node must be bound to that same address, which also means it is
///   on the Wi-Fi Direct link the connection came over.
/// - `expected_endpoint` is the endpoint ID the invitation named, if any.
/// - `info_urls` holds the URL of each agent info in `hello.agent_infos`, in
///   order, as decoded by the caller. Each must be the hello's direct URL,
///   so a peer can not register agents reachable somewhere else, such as a
///   relay or another machine.
pub fn check_hello(
    hello: &PeerHello,
    observed_ip: IpAddr,
    expected_endpoint: Option<&EndpointId>,
    info_urls: &[Option<String>],
) -> Result<(), HelloError> {
    check_name(&hello.name).map_err(HelloError::Name)?;
    if hello.agent_infos.is_empty() {
        return Err(HelloError::NoAgentInfos);
    }
    if hello.agent_infos.len() > MAX_AGENT_INFOS {
        return Err(HelloError::TooManyAgentInfos(hello.agent_infos.len()));
    }
    if hello.addr.ip() != observed_ip {
        return Err(HelloError::AddressMismatch {
            claimed: hello.addr.ip(),
            observed: observed_ip,
        });
    }
    if let Some(expected) = expected_endpoint
        && *expected != hello.endpoint_id
    {
        return Err(HelloError::EndpointMismatch {
            expected: *expected,
            actual: hello.endpoint_id,
        });
    }
    for url in info_urls {
        let url = url.as_deref().ok_or(HelloError::NoUrl)?;
        let direct =
            DirectPeerUrl::parse(url).map_err(|_| HelloError::NotDirect(url.to_string()))?;
        if direct.endpoint_id != hello.endpoint_id || direct.addr != hello.addr {
            return Err(HelloError::UrlMismatch {
                url: url.to_string(),
                endpoint_id: hello.endpoint_id,
                addr: hello.addr,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direct_url::tests::test_endpoint_id;

    fn other_endpoint_id() -> EndpointId {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        EndpointId::from_bytes(signing.verifying_key().to_bytes()).unwrap()
    }

    fn hello() -> PeerHello {
        PeerHello {
            name: "ubuntu".into(),
            endpoint_id: test_endpoint_id(),
            addr: "192.168.137.20:47001".parse().unwrap(),
            agent_infos: vec!["{\"agentInfo\":\"…\",\"signature\":\"…\"}".into()],
        }
    }

    fn url_of(hello: &PeerHello) -> Vec<Option<String>> {
        vec![Some(
            DirectPeerUrl {
                addr: hello.addr,
                endpoint_id: hello.endpoint_id,
            }
            .to_string(),
        )]
    }

    fn observed() -> IpAddr {
        "192.168.137.20".parse().unwrap()
    }

    #[test]
    fn a_consistent_hello_passes() {
        let hello = hello();
        check_hello(&hello, observed(), None, &url_of(&hello)).unwrap();
        check_hello(
            &hello,
            observed(),
            Some(&test_endpoint_id()),
            &url_of(&hello),
        )
        .unwrap();
    }

    #[test]
    fn messages_round_trip_through_json() {
        for message in [
            Message::JoinHello(hello()),
            Message::HostHello(hello()),
            Message::Rejected {
                reason: "expired".into(),
            },
            Message::Ack,
        ] {
            let json = serde_json::to_string(&message).unwrap();
            assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), message);
        }
        let json = serde_json::to_value(Message::JoinHello(hello())).unwrap();
        assert_eq!(json["type"], "join_hello");
        assert_eq!(json["endpoint_id"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn rejects_a_node_bound_somewhere_other_than_where_it_connected_from() {
        let hello = hello();
        let err = check_hello(&hello, "10.0.0.5".parse().unwrap(), None, &url_of(&hello));
        assert!(
            matches!(err, Err(HelloError::AddressMismatch { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_an_endpoint_other_than_the_invitation_named() {
        let hello = hello();
        let err = check_hello(
            &hello,
            observed(),
            Some(&other_endpoint_id()),
            &url_of(&hello),
        );
        assert!(
            matches!(err, Err(HelloError::EndpointMismatch { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_agent_infos_for_another_endpoint_or_address() {
        let hello = hello();
        let other_endpoint = DirectPeerUrl {
            addr: hello.addr,
            endpoint_id: other_endpoint_id(),
        };
        let other_port = DirectPeerUrl {
            addr: "192.168.137.20:47999".parse().unwrap(),
            endpoint_id: hello.endpoint_id,
        };
        for url in [other_endpoint, other_port] {
            let err = check_hello(&hello, observed(), None, &[Some(url.to_string())]);
            assert!(
                matches!(err, Err(HelloError::UrlMismatch { .. })),
                "{url}: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_agent_infos_reachable_through_a_relay_or_ble() {
        let hello = hello();
        let id = hello.endpoint_id;
        for url in [
            format!("https://use1-1.relay.n0.iroh-canary.iroh.link./{id}"),
            format!("https://ble.invalid:443/{id}"),
        ] {
            let err = check_hello(&hello, observed(), None, &[Some(url.clone())]);
            assert!(
                matches!(err, Err(HelloError::NotDirect(_))),
                "{url}: {err:?}"
            );
        }
        let err = check_hello(&hello, observed(), None, &[None]);
        assert_eq!(err, Err(HelloError::NoUrl));
    }

    #[test]
    fn rejects_empty_or_oversized_agent_info_lists_and_bad_names() {
        let mut empty = hello();
        empty.agent_infos.clear();
        assert_eq!(
            check_hello(&empty, observed(), None, &[]),
            Err(HelloError::NoAgentInfos)
        );

        let mut many = hello();
        many.agent_infos = vec!["x".into(); MAX_AGENT_INFOS + 1];
        assert_eq!(
            check_hello(&many, observed(), None, &[]),
            Err(HelloError::TooManyAgentInfos(MAX_AGENT_INFOS + 1))
        );

        let mut named = hello();
        named.name = "Peer Connected: Windows".into();
        assert!(matches!(
            check_hello(&named, observed(), None, &url_of(&named)),
            Err(HelloError::Name(_))
        ));
    }
}
