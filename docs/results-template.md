# Results

Copy this file to `results/<date>.md` (git-ignored) or into an issue, and
fill it in per run. A milestone is PASS only with its evidence attached:
commands, output excerpts, capture summaries. Redact nothing except the
Wi-Fi passphrase, which hc-wfd never prints unless asked to.

## Run

| | |
|---|---|
| Date | |
| hc-wfd commit (`git rev-parse HEAD`) | |
| Windows | build, Wi-Fi adapter and driver version |
| Ubuntu | release, kernel, `iwlwifi` firmware |
| Distance, obstacles | |
| Other networks connected | Ethernet / home Wi-Fi / Tailscale, on each machine |

## Milestone 1: Wi-Fi Direct IP link

| Test | Result | Evidence |
|---|---|---|
| A. `hc-wfd probe` on Windows | | |
| A. `create`: group started, SSID read back | | |
| A. Group adapter address: before or after the client joined? Which subnet? | | |
| B. Ubuntu sees the SSID (BSSID, channel, signal) | | |
| C. Ubuntu joined with the QR credentials; DHCP lease (address, router) | | |
| D. `ubuntu-verify-link.sh`: route, association | | |
| D. `windows-verify-link.ps1`: route, adapter | | |

## Milestone 2: authenticated QR pairing

| Test | Result | Evidence |
|---|---|---|
| How the QR code reached Ubuntu (USB, photo, webcam, text) | | |
| E. Both sides `paired with …` | | |
| J. Reused QR code refused | | |
| J. Expired invitation refused (host, and joiner) | | |

## Milestone 3: Holochain over Wi-Fi Direct

| Test | Result | Evidence |
|---|---|---|
| F. `Peer Connected` on both; `direct IP path: true` | | |
| F. Capture: every UDP 47001 packet on the Wi-Fi Direct interface (Ubuntu count, Windows pktmon) | | |
| G. Windows → Ubuntu PASS (both reports) | | |
| G. Ubuntu → Windows PASS (both reports) | | |
| H. Internet and other networks off: RESULT PASS | | |
| I. Link broken for … s: recovery time, RESULT | | |
| K. 3 × 1 MB: time to read each | | |
| K. 4 MB | | |

## Notes

Anything unexpected: warnings, retries, firewall prompts, timings.
