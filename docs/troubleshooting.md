# Troubleshooting

hc-wfd names the stage that failed: `FAILED at '<stage>': <cause>`. Find the
stage below.

## Building

| Symptom | Fix |
|---|---|
| `make: *** No rule to make target 'install'` (libsodium) | The target directory path contains a space. Use `C:\hc-wifi-target` or `~/.cache/…`. |
| openssl-src fails on Windows | perl is Git's msys perl. Install Strawberry Perl; `windows-build.ps1` then uses it. |
| aws-lc-sys fails on Windows | Install NASM (`winget install NASM.NASM`), or set `AWS_LC_SYS_PREBUILT_NASM=1`. |
| `--locked` refuses: `Cargo.lock` needs updating | Do not update it on that machine; report it. The lock pins Holochain 0.7.0's tested tree. |
| Errors in `crates/wifi-direct` on Windows | That code was type-checked against `windows` 0.62.2 for `x86_64-pc-windows-gnu` on Linux, but never built with MSVC. Send the error. |
| `holochain_state` errors about `CapGrant` | Something resolved `holochain_zome_types` 0.7.1. Restore `Cargo.lock` from git. |

## Windows: Starting Wi-Fi Direct group

| Symptom | Cause, fix |
|---|---|
| `RadioNotAvailable` | Wi-Fi is off, in airplane mode, or the driver lacks Wi-Fi Direct. `netsh wlan show wirelesscapabilities` should list "WiFi Direct GO: Supported". |
| `ResourceInUse` | Mobile Hotspot, or another Wi-Fi Direct group (Miracast, Nearby Share, Phone Link), is using the GO port. Turn them off. This PC has one GO port. |
| `did not start the group within 20s` | Restart the "WLAN AutoConfig" service, or toggle Wi-Fi off and on. |
| Windows reports a different SSID or passphrase | It is noted, and the QR code carries the values Windows reported. |
| `address not assigned yet` | Normal if Windows assigns it only when a client associates. The QR code then has no host identity, and pairing still works. |
| `this is a link-local address` | Windows is not running DHCP for the group. On Ubuntu: `join --ipv4 link-local --host-ip <that address>`. |

## Ubuntu: Joining Wi-Fi Direct

| Symptom | Cause, fix |
|---|---|
| `no network named DIRECT-hc-… found` | The group is not up, or is out of range. Check Windows still shows the QR code. A 5 GHz group needs a 5 GHz-capable client; `iw list` shows the bands. |
| `NetworkManager is not running` or `Wi-Fi is switched off` | `sudo systemctl start NetworkManager`; `nmcli radio wifi on`. |
| `Insufficient privileges` from `nmcli` | Run `join` from a terminal in the desktop session. Over SSH, polkit does not grant NetworkManager rights. The profile is owned by your user, so it needs no admin rights there. |
| `… is in use by '…'; pass --yes` | Not interactive. Pass `--yes` to allow leaving that Wi-Fi network. |
| `joining DIRECT-hc-…` fails with `Secrets were required` or a timeout | Wrong or old QR code: Windows makes a new passphrase every session. Use the current code. |
| `got no IPv4 address within 45s` | No DHCP on the group. Use `--ipv4 link-local --host-ip <Windows address>`, or a fixed `--ipv4 192.168.137.200/24 --host-ip 192.168.137.1` on Windows' subnet. |
| `the route to … goes through …` | Another network, often the home LAN or a VPN, uses the same subnet. Disconnect it. |
| Home Wi-Fi did not come back | `hc-wfd leave`, or `nmcli connection up id '<name>'`. |
| A leftover `hc-wfd-*` profile | `hc-wfd leave` deletes all of them. |

## Waiting for the other computer, and Pairing with peer

| Symptom | Cause, fix |
|---|---|
| `… is not answering yet` (Ubuntu), then `Connection refused` or a timeout | Windows Firewall is blocking TCP 47002. Run `create` elevated, so hc-wfd adds its scoped rules, or add them with `scripts\windows-firewall.ps1 -Add -LocalIp <GO address>`. If you once clicked Cancel on Windows' "allow access" prompt, a block rule for hc-wfd.exe overrides any allow rule; `windows-check.ps1` lists it. Delete it in "Windows Defender Firewall with Advanced Security". |
| `Connection refused` straight after a pairing | The invitation was used, so the port is closed. Expected for a reused QR code. |
| `authentication failed` | The QR code is from another session, or was damaged. Use the current one. |
| `says its node is at X, but its pairing connection came from Y` | The joiner's Holochain endpoint and its pairing connection used different interfaces. Report it, with `ip route` output. |
| `the invitation names host endpoint …` | The host restarted after the QR code was made. Use the new code. |
| `both nodes are named …` | Give one `--name`. |
| `the invitation expired` | Start a new session, with a longer `--expires-min` if needed. |
| `revoked` after 5 failures | Something kept failing to pair; look at the earlier `attempt from … failed` lines. Start a new session. |

## Holochain connected, and Synchronizing

| Symptom | Cause, fix |
|---|---|
| Paired, but no `Peer Connected` for minutes | UDP 47001 is blocked. On Windows, the scoped rules cover it. On Ubuntu with ufw: `scripts/ubuntu-firewall.sh open 47001`. Check with the verify scripts and a capture. |
| `no manifest from … within …s` | The peer never got its notes across. Compare both reports. If one side has `connected never`, see the row above. |
| `no receipt from …` | The peer read our notes only partly, or stopped before writing its receipt. Its own report says which. Raise `--linger-s` on the side that finishes first. |
| `CORRUPT` | A note arrived with the wrong bytes. Keep both logs and report it. |
| `database is locked` errors in the log | Transient SQLite contention between Holochain workflows, worst in debug builds. Holochain retries them. Not a failure unless the result is. |
| Large notes time out | `--request-timeout-s 180` on both sides. |
| `rejoined, but with … instead of …` | Ubuntu got a new address after a reconnect, so its endpoint is bound to the old one. Start a new session. |

## Getting more detail

`RUST_LOG=warn,hc_wfd=info,kitsune2_transport_iroh=debug,iroh=info`, with
stderr saved to a file. The iroh debug log shows each connection's remote
address and path.
