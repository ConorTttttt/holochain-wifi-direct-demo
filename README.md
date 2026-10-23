# Holochain over Wi-Fi Direct, with QR-code pairing

A Windows PC and an Ubuntu PC share a Holochain DHT over a direct Wi-Fi
Direct link, with no router, no internet, no relay and no bootstrap server.
They pair with a single QR code.

```text
Windows: hc-wfd create                          Ubuntu: hc-wfd join --qr-image invite-qr.png
  starts a Wi-Fi Direct group owner               reads the QR code, checks it
  starts Holochain on the group's address         joins the group (NetworkManager)
  shows a QR code: SSID, passphrase,              finds Windows at the group's DHCP router
    one-time secret, session, endpoint            starts Holochain on its link address
  waits for one authenticated pairing    <-----   pairs: Noise PSK handshake, swaps agent infos
  Peer Connected: Ubuntu                          Peer Connected: Windows
  writes notes, reads Ubuntu's, receipts <=====>  writes notes, reads Windows', receipts
  RESULT PASS, both directions                    RESULT PASS, both directions
```

## Status

| Milestone | State | Evidence |
|---|---|---|
| Pairing library: QR format, single-use expiring invitations, Noise `NNpsk0` channel, hello checks | **READY** | 47 tests pass on Ubuntu (`cargo test -p hcwfd-pairing`), including replay, wrong secret, wrong session, reuse, expiry, impostor host and recovery after a failure |
| QR generation (PNG and terminal) and import (PNG, phone photo, pasted text) | **READY** | PNG round trip and a noisy, off-centre 3000×2200 "photo" decode in tests |
| Webcam scanning | **NOT YET VERIFIED** | Uses `zbarcam` (zbar-tools). Not installed on the Ubuntu PC, which has no webcam |
| One-way QR pairing, mutual registration, two Holochain 0.7.0 conductors exchanging entries both ways over direct iroh QUIC, no relay, no bootstrap | **READY on loopback** | `scripts/loopback-test.sh`: 2 × 50 kB each way, byte-for-byte PASS on both sides; one UDP socket per node; pairing port closed after use; no secret in any log. Also 3 × 1 MB each way, PASS. **Not a Wi-Fi Direct result.** |
| Ubuntu joiner (`nmcli`) | **PASS** (2026-10-23) | Joined the Windows group as a legacy client, got DHCP, rejoined the home Wi-Fi afterwards and left no profile behind |
| Windows group owner and adapter address | **PASS** (2026-10-23) | Group `DIRECT-hc-…` on 5 GHz (5745 MHz). The adapter had `192.168.137.1` when the group started, so the QR code carried the host's endpoint |
| Windows build of `hc-wfd.exe` | **PASS** | Built and ran `create`. Windows Firewall rules: not yet checked from the Windows side |
| Milestone 1: real Windows–Ubuntu Wi-Fi Direct IP link | **PASS** (2026-10-23) | Ubuntu: associated to the `DIRECT-` SSID (`iw`: Wi-Fi 6, 80 MHz, −54 dBm); DHCP `192.168.137.24/24` from Windows; `ip route get 192.168.137.1` → `dev wlp0s20f3`, no gateway; default route stayed on Ethernet. Ping: no reply (Windows blocks ICMP; not required). `windows-verify-link.ps1` not yet run |
| Milestone 2: authenticated QR pairing over Wi-Fi Direct | **E, J PASS** except host-side expiry (run C) | Run 1 and run A (2026-10-23): QR PNG imported on Ubuntu; Noise pairing to `192.168.137.1:47002`; endpoint matched the QR code; pairing port closed afterwards. J: a second join with a copy of the used QR code reached the Wi-Fi group but could not pair (port closed); a QR code 10 min past expiry was refused by Ubuntu. Not yet run: Windows refusing an expired code (`create --expires-min 1`) |
| Milestone 3: Holochain DHT both ways over Wi-Fi Direct | **F, G, H, K PASS**; **capture and I not yet run** | Run 1: 1 × 1 kB each way, `Peer Connected` after 4–5 s, `direct IP path: true`, both reports PASS. Run A, Windows off every other network (its log: `failed to retrieve default route: NoRoute`; Ubuntu kept Ethernet, which does not reach Windows): 3 × 1 MB each way, every byte matched, about 6 MB over the link each way, group on 2.4 GHz, both reports PASS. Still needed for the final PASS: a packet capture on the Wi-Fi Direct interface, and reconnection (I) with the outage fixes below |
| Recovery from link outages (kitsune2 `c700b35`) | **READY**, simulated | Run A's log showed Windows' iroh endpoint dying for good after its group adapter's address briefly went away. Fixed in kitsune2 in three parts (loopback socket, no blame for a local outage, fresh connections when the address returns; see [docs/architecture.md](docs/architecture.md#link-outages)). `scripts/outage-test.sh`: two nodes in separate network namespaces, each node's address removed for 45 s in turn, 10 notes each way: **5 of 5 runs PASS** (before the third part: 1 of 5 failed). **Not a Wi-Fi Direct result**; hardware test I is still to run |
| Native P2P negotiation on Ubuntu (mode A), Android, iOS, GUI | Not built | [docs/future.md](docs/future.md) |

Answered by the first hardware run (2026-10-23): Windows gives the group
adapter its address (`192.168.137.1/24`) when the group starts, and its group
owner serves DHCP to legacy clients. The fallbacks for other behaviour
(`--ipv4 link-local --host-ip …`) remain.

**To test on the two machines, follow
[docs/windows-ubuntu-test.md](docs/windows-ubuntu-test.md).**

## How it works

- **Wi-Fi Direct.** Windows is the group owner, through WinRT's
  `WiFiDirectAdvertisementPublisher` in legacy mode, the API of Microsoft's
  WiFiDirectLegacyAP sample. It is not Mobile Hotspot and not a soft AP.
  Ubuntu joins as an ordinary WPA2 client with a temporary NetworkManager
  profile that never becomes the default route and never stores the
  passphrase. See [docs/architecture.md](docs/architecture.md).
- **Holochain.** Holochain 0.7.0, kitsune2 0.5.2 and iroh 1.2.0, the same
  versions as the BLE demo. One iroh endpoint, owned by kitsune2, on iroh's
  native UDP/QUIC. kitsune2 0.5.2 can only run with a relay, so this uses
  [ConorTttttt/kitsune2 `feature/wifi-direct-0.5`](https://github.com/ConorTttttt/kitsune2/tree/feature/wifi-direct-0.5)
  (`c700b35`). It adds one setting, `directBindAddr`: peer traffic goes
  through one socket bound to the Wi-Fi Direct address, there is no relay,
  and peer URLs take the form `http://<ip>:<port>/direct/<endpoint-id>`. A
  loopback socket beside it keeps the endpoint alive while the link address
  is briefly gone (found in the hardware run; see
  [docs/architecture.md](docs/architecture.md#link-outages)). The BLE
  transport is not used; its feature is never turned on.
- **Pairing.** The QR code carries the group's credentials, a 32-byte
  single-use secret, a session ID and, when known, the host's endpoint ID.
  The joiner proves it holds the secret through a Noise PSK handshake, which
  also proves the host holds it. Each side then checks the other's signed
  agent infos against the connection, and registers them. See
  [docs/qr-pairing.md](docs/qr-pairing.md), which also covers what sharing a
  QR code gives away.
- **The test.** The BLE demo's probe hApp, byte for byte. Each session uses
  its own network seed. Data notes are followed by a manifest, and each side
  writes a receipt, so each machine prints PASS or FAIL for both directions.

## Quick start

Windows (PowerShell; elevated, so hc-wfd can add its scoped firewall rules):

```powershell
cd C:\irohble
git clone https://github.com/ConorTttttt/holochain-wifi-direct-demo.git
cd holochain-wifi-direct-demo
powershell -ExecutionPolicy Bypass -File scripts\windows-build.ps1
C:\hc-wifi-target\release\hc-wfd.exe probe
C:\hc-wifi-target\release\hc-wfd.exe create
```

Ubuntu:

```bash
git clone https://github.com/ConorTttttt/holochain-wifi-direct-demo.git
cd holochain-wifi-direct-demo
scripts/ubuntu-check.sh && scripts/ubuntu-build.sh
~/.cache/holochain-wifi-direct-demo/target/release/hc-wfd join --qr-image /path/to/invite-qr.png
```

With no command, `hc-wfd` shows a menu: Create Holochain Network, Join
Holochain Network, Leave, Check this machine.

The Ubuntu PC has no camera, so the QR code shown on Windows has to travel to
it: copy the PNG on a USB stick, or send it a phone photo of the screen.
`--camera` scans with a webcam through zbarcam. `create --print-payload` with
`join --payload-file` moves it as text.

## Commands

| Command | Does |
|---|---|
| `hc-wfd create` | Windows. Group owner, conductor, QR code, pairing, exchange. Options: `--notes`, `--note-bytes`, `--expires-min`, `--firewall auto\|on\|off`, `--print-payload`, `--link-ip`, `--zero-arc`, … |
| `hc-wfd join` | Ubuntu. `--qr-image FILE`, `--payload-file FILE`, `--camera`, or interactive. `--yes` to switch Wi-Fi without asking; `--ipv4 dhcp\|link-local\|<cidr>`; `--host-ip`; `--stay-joined` |
| `hc-wfd leave` | Ubuntu. Deletes `hc-wfd-*` profiles and reconnects the Wi-Fi network joining replaced |
| `hc-wfd probe` | Windows: starts a group for two seconds. Ubuntu: NetworkManager, Wi-Fi, zbarcam, ufw |
| `hc-wfd decode FILE\|TEXT` | Shows what a QR code holds, secrets hidden |

`--no-wifi --link-ip <ip>` runs either side without Wi-Fi, for loopback and
LAN regression tests. Such runs are labelled NOT Wi-Fi Direct.

## Layout

```text
crates/pairing         hcwfd-pairing: invitations, Noise channel, checks (no Holochain)
crates/wifi-direct     hcwfd-wifi: Windows group owner and firewall, Ubuntu nmcli joiner
crates/holochain-node  hc-wfd: the program, embedded conductor, QR codes, DHT exchange
happ/probe.happ        the probe hApp, identical to holochain-ble-demo's ble_probe.happ
dna/                   its zome sources (scripts/build-happ.sh)
scripts/               build, check, firewall, link verification, loopback tests
docs/                  architecture, QR pairing, hardware test, results, troubleshooting, future
```

## Build notes

- Pinned: `Cargo.lock` began as holochain-ble-demo's, so Holochain 0.7.0's
  tested tree is unchanged. The only additions are the QR and Noise crates
  (`qrcode`, `rqrr`, `image`, `snow`), and the BLE-only crates are not
  built. Always build with `--locked`, as the scripts do. Rust 1.99.0 comes
  from `rust-toolchain.toml`.
- Separate from the BLE project. On Windows, output goes to
  `C:\hc-wifi-target`, never `C:\hc-ble-target`. On Ubuntu it goes to
  `~/.cache/holochain-wifi-direct-demo/target`. Runtime data is in
  `%LOCALAPPDATA%\hc-wfd` or `~/hcw/<name>-<session>`. No BLE branch, build
  or data is touched.
- No spaces in build paths: libsodium's build fails under one.

## License

None chosen yet, as for holochain-ble-demo. The binary links Holochain 0.7.0
(CAL-1.0); it does not link the AGPL BLE stack.
