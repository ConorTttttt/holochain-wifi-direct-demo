# Architecture

```text
Windows PC: hc-wfd create                              Ubuntu PC: hc-wfd join
┌───────────────────────────────────────┐             ┌───────────────────────────────────────┐
│ Wi-Fi Direct group owner (WinRT)      │  Wi-Fi      │ NetworkManager profile hc-wfd-<sid>   │
│  WiFiDirectAdvertisementPublisher     │  Direct     │  legacy WPA2-PSK station, DHCP,       │
│  LegacySettings: SSID + passphrase    │◄═══group═══►│  never-default, PSK not stored        │
│  adapter "Wi-Fi Direct Virtual        │  no router  │  wlp0s20f3                            │
│  Adapter #N": 192.168.137.1 (example) │  no AP      │  192.168.137.x (DHCP from Windows)    │
├───────────────────────────────────────┤             ├───────────────────────────────────────┤
│ pairing service TCP <GO-ip>:47002     │◄─ Noise ────│ pairing client, from <own-ip>         │
│  open until one peer pairs            │   NNpsk0    │                                       │
├───────────────────────────────────────┤             ├───────────────────────────────────────┤
│ Holochain 0.7.0 conductor             │             │ Holochain 0.7.0 conductor             │
│  kitsune2 0.5.2 (feature/wifi-direct) │             │  kitsune2 0.5.2 (feature/wifi-direct) │
│  one iroh 1.2.0 endpoint              │◄─ QUIC ────►│  one iroh 1.2.0 endpoint              │
│   directBindAddr <GO-ip>:47001        │   UDP,      │   directBindAddr <own-ip>:47001       │
│   relay disabled, no bootstrap        │   TLS 1.3   │   relay disabled, no bootstrap        │
└───────────────────────────────────────┘             └───────────────────────────────────────┘
```

## Pieces

| Crate | Path | Role |
|---|---|---|
| `hcwfd-pairing` | [crates/pairing](../crates/pairing) | Invitation format, single-use host session, Noise PSK channel, hello checks. Pure Rust, no Holochain. |
| `hcwfd-wifi` | [crates/wifi-direct](../crates/wifi-direct) | Windows group owner and adapter address (WinRT, IP Helper), scoped Windows Firewall rules (`netsh`), Ubuntu joiner (`nmcli`, `ip`, `iw`). |
| `hc-wfd` | [crates/holochain-node](../crates/holochain-node) | The program: menu, `create`, `join`, `leave`, `probe`, `decode`; the embedded conductor; QR drawing and reading; the two-way DHT exchange. |

## Wi-Fi Direct

**Windows is the group owner**, through WinRT's
`WiFiDirectAdvertisementPublisher` with `LegacySettings.IsEnabled = true`,
`IsAutonomousGroupOwnerEnabled = true` and discoverability `Normal`. This is
the API of Microsoft's
[WiFiDirectLegacyAP](https://github.com/microsoft/Windows-classic-samples/tree/main/Samples/WiFiDirectLegacyAP)
sample. It makes Windows the owner of a real Wi-Fi Direct group, and lets a
station with no Wi-Fi Direct support join it with an ordinary WPA2 SSID and
passphrase. It is neither Mobile Hotspot nor a soft AP; this Windows PC
reports "Soft AP: Not supported" and "Wi-Fi Direct GO: Supported", which is
why this API fits it.

- All WinRT objects live on one thread `hcwfd-wifi` owns (MTA), so nothing
  crosses apartments. `StatusChanged` updates the status the program
  watches. `Aborted` with `RadioNotAvailable` or `ResourceInUse` is reported
  with its likely cause.
- After `Start`, the SSID and passphrase are read back from
  `LegacySettings`, and those values go into the QR code.
- A `WiFiDirectConnectionListener` records P2P connection requests, which
  legacy clients never make. They are logged as ignored; native P2P
  negotiation (mode A below) is not built.
- The group adapter's address comes from `GetAdaptersAddresses`: the first
  IPv4 address on a "Wi-Fi Direct" adapter that was not up before the group
  started. See [qr-pairing.md](qr-pairing.md#the-address-problem) for when it
  is not known yet.
- The group stops when hc-wfd exits, or at once on `Ctrl-C`.

**Ubuntu joins as a legacy client** through NetworkManager. The profile
`hc-wfd-<session>` has autoconnect off, `ipv4.never-default`, ignores the
group's DNS, has IPv6 off, and does not store the passphrase
(`wifi-sec.psk-flags 2`). The passphrase reaches NetworkManager through a 0600
`passwd-file` that is deleted as soon as `nmcli connection up` returns.
Joining uses the interface's single station slot (`managed <= 1` on the HP's
Intel CNVi), so a current Wi-Fi connection on it drops. hc-wfd scans for the
group first, then warns and asks before switching, and reconnects the old
network when it exits (`hc-wfd leave` does so after a crash). Wired
connections are not touched. While the exchange runs, a watchdog re-activates
the profile if the link drops.

### Modes

| Mode | Status | What it is |
|---|---|---|
| **B. Windows group owner, Ubuntu legacy client** | Built here | Windows runs Wi-Fi Direct. Ubuntu sees a WPA2 network whose owner is a Wi-Fi Direct GO. No router, AP, hotspot or internet. |
| A. Native P2P on both sides | Not built | Ubuntu's `wpa_supplicant` (`p2p_find`, `p2p_connect … pbc`) negotiates as a P2P client. Windows would have to accept it with `WiFiDirectConnectionListener` and `WiFiDirectDevice.FromIdAsync`, with pairing through `DeviceInformationCustomPairing`, which needs UI or a packaged app context. Ubuntu's P2P-client mode would leave its station slot free, so home Wi-Fi would stay up. |
| C. Mobile Hotspot | Not used | Not Wi-Fi Direct, and can not run alongside a GO. |
| D. Router or LAN | Regression only | `--no-wifi --link-ip …` runs. They are labelled "NOT a Wi-Fi Direct link" in every report. |

## Holochain and iroh

Holochain 0.7.0 does not build its own transport. `holochain_p2p` asks
kitsune2 0.5 for its default builder, whose transport is
`kitsune2_transport_iroh`, which owns exactly one `iroh::Endpoint`. Stock
kitsune2 0.5.2 can not run without a relay: it learns its own peer URL only
from the relay, and dials peers only through the relay in their URL. So
[ConorTttttt/kitsune2 `feature/wifi-direct-0.5`](https://github.com/ConorTttttt/kitsune2/tree/feature/wifi-direct-0.5)
(commit `b340822`, one commit on top of the BLE branch) adds one setting,
`irohTransport.directBindAddr`:

- the endpoint is built with `RelayMode::Disabled`, `clear_ip_transports()`
  and `bind_addr(<directBindAddr>)`, so it has one UDP socket, bound to the
  Wi-Fi Direct address, and no relay or address lookup;
- its peer URL is `http://<ip>:<port>/direct/<endpoint-id>`, known as soon as
  it binds;
- dialling such a URL gives iroh exactly that socket address and endpoint ID:
  `EndpointAddr::from_parts(id, [TransportAddr::Ip(addr)])`;
- network stats report the backend as `iroh-direct`. QUIC, TLS 1.3 with the
  endpoint ID, kitsune2's preflight, connection handling and frame limits are
  unchanged. BLE's `https://ble.invalid:443/…` form is not used, and the
  `ble` feature is never turned on in this project.

hc-wfd sets it through `network.advanced.irohTransport.directBindAddr`. It
gives Holochain a bootstrap URL of `http://127.0.0.1:1/`, which refuses at once
with no DNS lookup and no traffic off the machine; Holochain 0.7 can switch
bootstrap off only in test builds. Each session installs the probe DNA with
network seed `hc-wfd-<session_id>`. Peers are introduced only by the pairing
exchange's `add_agent_infos`.

Agent infos are re-signed every minute with 18 of their 20 minutes left, so
after a link drop a fresh one exists within about 2 minutes. This matters
because kitsune2 marks a peer that failed to connect as unresponsive until a
newer agent info for it arrives.

## The DHT exchange

Reused from holochain-ble-demo: the same probe hApp, embedded byte for byte,
and the same byte pattern. A note's body is its label repeated, and the label
ends in the body size, so truncation and corruption are both caught. On top
of that:

1. Each node writes `--notes` data notes, then a `sent:<name>` manifest with
   each note's label, size and SHA-256.
2. Each node reads its peer's manifest and every note it lists, and checks
   each body byte for byte against the pattern and against the manifest's
   SHA-256.
3. It then writes `receipt:<name>`, listing what it read.
4. Each node reports both directions:
   - peer to me, from its own reads;
   - me to peer, from the peer's receipt, checked against what it wrote.

```text
   ubuntu -> windows   PASS  2 note(s), 100000 bytes, every byte matched (read here)
  windows -> ubuntu    PASS  2 note(s), 100000 bytes, every byte matched (ubuntu's receipt)
RESULT    PASS (both directions over the Wi-Fi Direct link)
```

"Peer Connected: Ubuntu" or "Windows" is printed when kitsune2's transport
stats first show an open connection whose remote endpoint ID is the paired
peer's: a real QUIC connection, not the QR scan or the pairing exchange.

## What proves the traffic used Wi-Fi Direct

1. **By construction.** The only UDP socket is bound to the Wi-Fi Direct
   address, there is no relay, and the route to the peer is the Wi-Fi Direct
   interface with no gateway. Ubuntu's join checks the route itself.
2. **The reports** show `transport iroh-direct at http://<link-ip>:47001/…`,
   `direct IP path: true`, and non-zero bytes for the connection to the peer.
3. **Independently:** `scripts/ubuntu-verify-link.sh` captures with
   `tcpdump -i any` and counts packets per interface. On Windows,
   `scripts\windows-verify-link.ps1` uses pktmon counters per adapter and
   saves a pcapng. Every packet on UDP 47001 must be on the Wi-Fi Direct
   interface.
4. **With the other paths gone:** test H disconnects Ethernet and Wi-Fi
   infrastructure on both machines and runs again.
