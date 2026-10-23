# Hardware test: Windows and Ubuntu over Wi-Fi Direct

Three milestones, in order. Do not mark one PASS without its evidence, and
record every run in [results-template.md](results-template.md).

| Milestone | Tests | Evidence |
|---|---|---|
| 1. A real Windows–Ubuntu Wi-Fi Direct IP link | A to D | `probe` PASS; Ubuntu associated to a `DIRECT-` SSID; route check; verify scripts |
| 2. Authenticated QR pairing | E, J | both sides print `Pairing with peer: paired with …`; reuse and expiry are refused |
| 3. Bidirectional Holochain DHT exchange | F, G, H, I, K | `Peer Connected` and `RESULT PASS` on both sides; packet capture on the Wi-Fi Direct interface; the same with internet off |

A loopback, Ethernet, home Wi-Fi, Mobile Hotspot or Tailscale result is
never a Wi-Fi Direct result. hc-wfd labels `--no-wifi` runs "NOT a Wi-Fi
Direct link".

## 0. Setup

### Windows (PowerShell)

Prerequisites, as for the BLE build: Rust with the MSVC host
(https://rustup.rs), Visual Studio Build Tools with "Desktop development
with C++" and a Windows SDK, Strawberry Perl, NASM (`winget install
NASM.NASM`), Git, and internet for the build. Mobile Hotspot must be off.

```powershell
mkdir C:\irohble -Force | Out-Null
cd C:\irohble
git clone https://github.com/ConorTttttt/holochain-wifi-direct-demo.git
cd C:\irohble\holochain-wifi-direct-demo
powershell -ExecutionPolicy Bypass -File scripts\windows-check.ps1
powershell -ExecutionPolicy Bypass -File scripts\windows-build.ps1
$exe = "C:\hc-wifi-target\release\hc-wfd.exe"
& $exe --version
```

The build goes to `C:\hc-wifi-target` and never touches `C:\hc-ble-target`.
The first build takes 20 to 40 minutes.

### Ubuntu

```bash
sudo apt install build-essential pkg-config perl iw tcpdump    # zbar-tools too, for a webcam
mkdir -p ~/src && cd ~/src
git clone https://github.com/ConorTttttt/holochain-wifi-direct-demo.git
cd holochain-wifi-direct-demo
scripts/ubuntu-check.sh
scripts/ubuntu-build.sh
BIN=$HOME/.cache/holochain-wifi-direct-demo/target/release/hc-wfd
$BIN --version
scripts/loopback-test.sh      # regression on this machine only: NOT Wi-Fi Direct
```

## 1. The Wi-Fi Direct link

### A. Windows starts a group owner

```powershell
& $exe probe
```

Pass: `PASS  this adapter can host a Wi-Fi Direct group with legacy clients`.
On `Aborted (RadioNotAvailable…)`, Wi-Fi is off or the driver lacks Wi-Fi
Direct. On `ResourceInUse`, Mobile Hotspot or another group is running.

Then start the real session. Use an elevated PowerShell (Run as
administrator), so hc-wfd can add its scoped firewall rules:

```powershell
& $exe create
```

Note what it prints:

- `Wi-Fi Direct group started: SSID DIRECT-hc-……`
- `address   <WIN_IP>/<prefix> on '<adapter>' (Microsoft Wi-Fi Direct Virtual Adapter #N)`,
  or that the address is not assigned yet;
- `firewall  added 'hc-wfd-<session>'…`;
- the QR code, in the terminal and in the Photos app, and its PNG path under
  `%LOCALAPPDATA%\hc-wfd\windows-<session>\invite-qr.png`.

Record whether the address appeared before or after the client joined. It
answers the open question of when Windows assigns it.

### B. Ubuntu sees the group

```bash
nmcli -f SSID,BSSID,CHAN,FREQ,SIGNAL device wifi list --rescan yes | grep DIRECT-hc
```

Pass: the SSID from A is listed. `hc-wfd join` scans for it too.

### C. Ubuntu joins with the QR credentials

Get the QR code to Ubuntu. The HP Pro Mini has no webcam, so a QR code on the
Windows screen can not be scanned directly. Use one of:

- **USB stick:** copy the PNG path hc-wfd printed, then
  `$BIN join --qr-image /media/$USER/<stick>/invite-qr.png`.
- **Phone photo:** photograph the QR code on the Windows screen, send the
  photo to Ubuntu, then `$BIN join --qr-image ~/Downloads/<photo>.jpg`.
  hc-wfd decodes photos and scales large ones down.
- **Webcam** (a USB one, with `sudo apt install zbar-tools`):
  `$BIN join --camera`.
- **Text:** start Windows with `create --print-payload`, move the `HCWFD1:…`
  line into a file on Ubuntu, then `$BIN join --payload-file invite.txt`.

Whichever route you use carries the live passphrase and pairing secret.
Delete the copy afterwards.

```bash
$BIN join --qr-image <file>
```

It scans for the SSID, warns that `wlp0s20f3` will leave its current Wi-Fi
network, and asks before switching (`--yes` skips the question; Ethernet
stays up). Then it joins.

Pass:

- `wifi      found DIRECT-hc-… (BSSID …)`
- `==> Wi-Fi connection established: wlp0s20f3 192.168.x.y/24 on DIRECT-hc-…, BSSID …; host <WIN_IP> directly on wlp0s20f3, no gateway`

Record whether Windows gave Ubuntu an address by DHCP, and from which subnet.

If ufw is enabled on Ubuntu, open the Holochain port on the link now, in a
second terminal. Windows may dial Ubuntu too:

```bash
scripts/ubuntu-firewall.sh open 47001
```

### D. IP connectivity over Wi-Fi Direct

While `join` runs, in a second Ubuntu terminal:

```bash
scripts/ubuntu-verify-link.sh <WIN_IP> 47001 30
```

On Windows, in a second elevated PowerShell:

```powershell
cd C:\irohble\holochain-wifi-direct-demo
powershell -ExecutionPolicy Bypass -File scripts\windows-verify-link.ps1 -PeerIp <UBUNTU_IP> -Port 47001 -Seconds 30
```

Pass: both report the route on the Wi-Fi Direct interface with no gateway,
and hc-wfd's single UDP socket on the link address. Ping may fail, because
Windows blocks ICMP by default; that is not required. Milestone 1 passes with
A to D.

## 2. Pairing

### E. Authenticated peer exchange

Same run. Pass, on Ubuntu:
`==> Pairing with peer: authenticating with <WIN_IP>:47002…` followed by
`pairing   paired with windows (endpoint ……, node at <WIN_IP>:47001)`. On
Windows: `pairing   connection from <UBUNTU_IP>:…` followed by
`==> Pairing with peer: paired with ubuntu (…)`.

A failure says why: authentication failed (a wrong or old QR code), the
address or endpoint did not match, the invitation expired, or the port was
not answering (the firewall).

### J. Expired and reused invitations are refused

- **Reuse:** after a successful pairing, keep a copy of the QR image and run
  a second joiner with it (`$BIN join --qr-image <copy> --name intruder --yes`).
  Pass: it fails at `Pairing with peer`; the host closed its pairing port when
  the first pairing completed.
- **Expiry:** `& $exe create --expires-min 1`, then wait 70 seconds before
  joining. Pass: Windows ends with
  `FAILED at 'Waiting for the other computer': the invitation expired`, and
  stops the group. An Ubuntu joiner more than 5 minutes past the expiry
  refuses the code itself (`the invitation expired …s ago`).

## 3. Holochain over Wi-Fi Direct

### F and G. Direct QUIC connection and entries both ways

Same run. Pass, on both sides:

```text
    *  Peer Connected: Windows   *        (on Ubuntu; Ubuntu on Windows)
transport iroh-direct at http://<own link IP>:47001/direct/<endpoint id>
          peer <peer endpoint id> sent … B in … msgs, received … B in … msgs, direct IP path: true
   ubuntu -> windows   PASS  1 note(s), 1024 bytes, every byte matched (…)
  windows -> ubuntu    PASS  1 note(s), 1024 bytes, every byte matched (…)
RESULT    PASS (both directions over the Wi-Fi Direct link)
```

Run the verify scripts from D during the exchange, as evidence that the
packets crossed the Wi-Fi Direct interface. Ubuntu's capture must show every
UDP 47001 packet on `wlp0s20f3`. Windows' pktmon counters must show them on
the Wi-Fi Direct virtual adapter; open the pcapng in Wireshark with
`udp.port == 47001`. Milestone 3 needs this evidence as well as `RESULT PASS`.

To force on-demand fetches instead of publish and gossip, add `--zero-arc` on
one side; its reads then say `fetched on demand`.

### H. Internet off

Before starting a fresh session, disconnect every other network on both
machines:

- **Ubuntu:** `nmcli device disconnect eno1`, and `sudo tailscale down` if
  Tailscale runs. hc-wfd moves `wlp0s20f3` off the home Wi-Fi itself.
- **Windows:** unplug Ethernet, and in Settings > Network & internet > Wi-Fi,
  disconnect from the home network. Keep Wi-Fi itself on, because the group
  needs it.

Then run `create` and `join` as before, moving the QR image by USB stick.
Nothing else connects the machines. Pass: `RESULT PASS` on both sides.
Restore afterwards: `nmcli device connect eno1`, `sudo tailscale up`, replug
Ethernet, rejoin the Wi-Fi.

### I. Disconnect and reconnect

```powershell
& $exe create --notes 10 --note-interval-s 30 --timeout-s 1800
```

```bash
$BIN join --qr-image <file> --notes 10 --note-interval-s 30 --timeout-s 1800
```

Midway, break the link for about a minute: on Ubuntu,
`nmcli connection down hc-wfd-<session>`. Ubuntu's watchdog prints
`the Wi-Fi Direct link is down; rejoining` and reconnects within about 5
seconds. To test a longer outage, toggle Ubuntu's Wi-Fi with
`nmcli radio wifi off`, wait, then `on`.

Pass: notes written during and after the gap still arrive, and both sides
end `RESULT PASS`. Record how long recovery took. If Ubuntu comes back with a
different address, hc-wfd says so; its endpoint stays bound to the old one.
That is a known limit; start a new session.

### K. Larger entries

```powershell
& $exe create --notes 3 --note-bytes 1000000
```

```bash
$BIN join --qr-image <file> --notes 3 --note-bytes 1000000
```

Then try `--note-bytes 4000000`, the DNA's limit. Record the time to read
each note. If requests time out, add `--request-timeout-s 180` on both
sides.

## Logs

hc-wfd prints its progress on stdout and the logs on stderr. To keep both,
and more iroh detail:

```powershell
$env:RUST_LOG = "warn,hc_wfd=info,kitsune2_transport_iroh=debug,iroh=info"
& $exe create 1> create.out 2> create.err
```

```bash
RUST_LOG=warn,hc_wfd=info,kitsune2_transport_iroh=debug,iroh=info $BIN join --qr-image <file> > join.out 2> join.err
```

Logs carry agent keys, endpoint IDs and link addresses, but never the Wi-Fi
passphrase or the pairing secret. Keep them out of the repository.

## Cleaning up

- **Windows:** Ctrl-C, or let the run end. The group stops, the firewall
  rules go, and the QR image is deleted. After a crash, remove leftover rules
  in an elevated PowerShell:
  `powershell -ExecutionPolicy Bypass -File scripts\windows-firewall.ps1 -RemoveAll`.
- **Ubuntu:** the run deletes its `hc-wfd-*` profile and reconnects your
  previous Wi-Fi network. After a crash: `$BIN leave`. Also
  `scripts/ubuntu-firewall.sh close` if you opened ufw.
- **Data:** each session's conductor data is in
  `%LOCALAPPDATA%\hc-wfd\<name>-<session>` on Windows or `~/hcw/<name>-<session>`
  on Ubuntu. Delete old ones freely.

See [troubleshooting.md](troubleshooting.md) when something fails.
