# QR pairing

One QR code, shown on Windows, is everything Ubuntu needs: how to join the
Wi-Fi Direct group, where the pairing service is, and a secret that proves
both sides read the same code. After the Wi-Fi link is up, the two nodes swap
their full peer information over a channel authenticated by that secret.
Nothing is typed, and no endpoint ID is copied by hand.

Code: [`crates/pairing`](../crates/pairing). It has no Holochain dependency,
and its tests run in about a second (`cargo test -p hcwfd-pairing`).

## The invitation

The QR code holds `HCWFD1:` followed by the invitation as unpadded base64url
JSON: one token, about 570 characters, that survives a terminal, a text file
or a chat intact. The `1` in the prefix is the major format version.

```json
{
  "protocol": "holochain-wifi-direct",
  "version": 1,
  "session_id": "<16 random bytes, base64url>",
  "expires_at": 1761300000,
  "wifi": {
    "mode": "wifi-direct-legacy",
    "ssid": "DIRECT-hc-Xy12ab",
    "passphrase": "<24 random letters and digits>"
  },
  "pairing": {
    "secret": "<32 random bytes, base64url>",
    "port": 47002
  },
  "host": {
    "name": "windows",
    "endpoint_id": "<64 hex characters: the full 32-byte iroh endpoint ID>",
    "addr": "192.168.137.1:47001"
  }
}
```

| Field | Meaning |
|---|---|
| `session_id` | Names the session. Binds the pairing handshake, and seeds the session's DNA (`network_seed = hc-wfd-<session_id>`), so every pairing starts an empty DHT that only holders of this code can find. Not secret on its own. |
| `expires_at` | UNIX seconds. The host refuses to pair after it, by its own clock. The joiner also refuses an invitation more than 5 minutes past it, allowing for clock skew between the machines. |
| `wifi` | The group to join, as a legacy WPA2-PSK client. The SSID and passphrase are the ones Windows reports after starting the group, read back from `LegacySettings`, not just the ones requested. |
| `pairing.secret` | Single use. The Noise PSK of the pairing channel. Never sent over the network. |
| `pairing.port` | TCP port of the pairing service on the host's Wi-Fi Direct address. |
| `host` | Optional. Present when the host's conductor was already bound to its Wi-Fi Direct address when the code was made. The joiner then also requires the host's endpoint ID to match. |

Not in the invitation: any private key, the lair passphrase, conductor
state, or the host's agent infos.

### Decoding rules

[`Invite::decode`](../crates/pairing/src/invite.rs) refuses input longer than 2048 characters
before decoding it. It then checks `protocol` and `version` before anything
else, so a newer format gets "version N is not supported" rather than a parse
error, and an `HCWFD2:` prefix is reported the same way. It parses strictly:
unknown fields are rejected. It then checks every field:

- the SSID is 1 to 32 bytes;
- the passphrase is 8 to 63 printable ASCII characters;
- the secret is 32 bytes and the session ID 16;
- the endpoint ID is 64 hex characters and a valid ed25519 point that is not
  of small order;
- the host address is a specific IP and a non-zero port;
- the node name is short and plain;
- the invitation has not expired, and its expiry is not more than a day away.

The `Debug` output of every type redacts the passphrase and the secret, and
nothing logs the invitation text. The loopback test checks that neither
secret appears in either node's log.

## The address problem

Windows may not give the group's virtual adapter an address until the first
client associates, so the host's IP may not be known when the QR code has to
be shown. The design never depends on it:

1. The host starts the group, then waits up to 15 seconds
   (`--address-wait-s`) for a Wi-Fi Direct adapter that was not up before to
   get an IPv4 address. It reads this from `GetAdaptersAddresses`. Nothing is
   hard-coded, and a link-local address is accepted only if nothing else
   appears within 15 seconds.
2. If the address is there, the conductor starts bound to it, and the QR code
   carries `host` with the endpoint ID and address.
3. If not, the QR code goes out without `host`. The host keeps waiting for the
   address, and starts the conductor and pairing service once it appears.
4. The joiner, once on the group, takes the host's address from its DHCP
   lease: `routers`, then `dhcp_server_identifier`, then the gateway. It
   prefers the QR code's `host.addr` if that address is on the joined subnet.
   It then checks that the kernel routes that address over the Wi-Fi
   interface itself, with no gateway. Whatever subnet Windows picks on a
   given run, this works.
5. The pairing handshake authenticates whatever answered at that address. Only
   a holder of the secret can complete it, so a wrong host fails, whether it
   was found by mistake or by spoofing.

If Windows runs no DHCP server for the group (not expected; see
[windows-ubuntu-test.md](windows-ubuntu-test.md), test D), join with
`--ipv4 link-local --host-ip <Windows address>` or `--ipv4 <ip>/<prefix>`.

## The pairing exchange

The joiner connects over TCP from its own link address to `<host>:<port>`,
and the two run [Noise](https://noiseprotocol.org/noise.html)
`Noise_NNpsk0_25519_ChaChaPoly_SHA256`, using the
[`snow`](https://crates.io/crates/snow) implementation. The prologue is
`holochain-wifi-direct/1 pairing\0` followed by the session ID. The PSK is the
pairing secret.

```text
joiner                                             host
  -> psk, e                 (AEAD tag under the secret: joiner proves it)
  <- e, ee                  (host proves it too; session keys now fresh)
  -> JoinHello { name, endpoint_id, addr, agent_infos }
                            host checks it, verifies and registers the agent infos
  <- HostHello { … }   or   Rejected { reason }
     joiner checks it, verifies and registers the agent infos
  -> Ack                    host marks the invitation used and closes the port
```

Messages are JSON, at most 65519 bytes, in length-prefixed Noise transport
frames. Each side has a 20-second read and write timeout.

### What each side checks

[`check_hello`](../crates/pairing/src/messages.rs) and `Node::agent_info_urls` require:

- **Address consistency.** The peer's `addr` IP must be the source address of
  the pairing connection. The joiner connects from the address its Holochain
  endpoint is bound to. The host's is the address the joiner dialled.
- **Identity consistency.** Every agent info's URL must be exactly
  `http://<addr>/direct/<endpoint_id>` from the hello. A peer can not
  register agents reachable through a relay, over BLE, or at another address.
- **Endpoint pinning.** If the QR code named the host's endpoint ID, the
  host's hello must carry exactly that ID.
- **Agent infos.** Each must verify against its agent's ed25519 key, with the
  same verifier the conductor uses. It must be for this session's DNA, not a
  tombstone, and not expired. There must be 1 to 8 of them.
- Distinct, well-formed node names.

Only after all of that does a side call `add_agent_infos`. Each conductor then
knows the other's agent at its direct URL, and kitsune2 connects with no relay
and no bootstrap server.

### Security properties

| Threat | What stops it |
|---|---|
| Someone on the group without the QR code | Can not complete the handshake: their first message fails the AEAD check. |
| A recorded handshake replayed later | The host's fresh ephemeral key makes new session keys, so the replayed transport frames do not decrypt (tested). |
| A second pairing with the same QR code | The host consumes the invitation on `Ack` and closes the pairing port. A later attempt finds nothing listening. The single-use rule is also enforced in [`HostSession`](../crates/pairing/src/session.rs) (tested). |
| A stale QR code | The host enforces `expires_at` by its own clock, and the joiner refuses codes more than 5 minutes past it. |
| Guessing, or keeping the pairing port busy | The invitation is revoked after 5 failed attempts. Ctrl-C on the host revokes it at once. |
| Injecting someone else's agent | Agent infos must verify, be for this session's DNA, and point at the authenticated peer's own direct address and endpoint. |
| A rogue pairing service | It can not answer the handshake without the secret, so the joiner detects it (tested). |
| Secrets in logs or files | `Debug` redacts them. The QR PNG is user-only (mode 0600 on Linux; `%LOCALAPPDATA%` on Windows) and is deleted once pairing completes or the program exits. Ubuntu never stores the passphrase in its NetworkManager profile, and passes it to `nmcli` through a 0600 file that is deleted at once. |

A failed attempt does not use up the invitation: a joiner whose connection
drops before `Ack` simply tries again. The joiner retries a dropped
connection, or a port that is not listening yet, for `--pair-timeout-s`
(120 s). It never retries a refusal.

### Sharing a QR code: what it gives away

Treat the QR code as a key with a 10-minute lifetime (`--expires-min`):

- **Anyone who reads it can join the Wi-Fi Direct group** while the host
  runs: phone camera, shoulder-surfing, or a copy of the PNG. The group's WPA2
  passphrase is in it. Being on the group gives IP access to Windows' group
  adapter, limited by the scoped firewall rules to the two hc-wfd ports.
- **Anyone who reads it first can pair once**, instead of the intended
  machine. The intended machine then fails to pair, which makes the takeover
  visible. Stop the host (Ctrl-C), which revokes the code, and start a new
  session.
- **It names the session's DNA.** The network seed derives from the session
  ID, so someone holding the code, on the group, could install the same DNA
  and try to gossip with the host directly, as any Holochain peer on an open
  network can. Holochain's membrane is the DNA itself. This demo's probe DNA
  admits anyone.

So show the code only to the machine that should join, let it expire, and
start a new session rather than reusing one. Do not commit QR images or
invitation text; `.gitignore` excludes the usual names.

## Two-way QR codes

Not implemented, and not needed. The authenticated return channel carries
Ubuntu's peer information to Windows, so a second QR code shown on Ubuntu
would add nothing. If the pairing channel itself can not work on some
network, for example because the host's TCP port is blocked, the fix is the
firewall rule (see [troubleshooting.md](troubleshooting.md)), not a second
code.
