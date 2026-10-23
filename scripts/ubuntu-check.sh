#!/usr/bin/env bash
# Check that this Ubuntu machine can build hc-wfd and join a Windows
# Wi-Fi Direct group as a legacy client. Changes nothing.
#
# Usage: scripts/ubuntu-check.sh [IFNAME]
#
# Prints PASS, WARN, FAIL or INFO per check and exits 1 if anything FAILed.
set -uo pipefail
failures=0
pass() { echo "PASS  $*"; }
warn() { echo "WARN  $*"; }
fail() { echo "FAIL  $*"; failures=$((failures + 1)); }
info() { echo "INFO  $*"; }

echo "== Build tools"
for tool in cargo rustup cc pkg-config perl make; do
  if command -v "$tool" > /dev/null; then pass "$tool"; else fail "$tool missing (sudo apt install build-essential pkg-config perl; Rust from https://rustup.rs)"; fi
done
channel="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$(dirname "$0")/../rust-toolchain.toml")"
if rustup toolchain list 2> /dev/null | grep -q "^$channel"; then pass "Rust $channel installed"; else warn "Rust $channel not installed yet; rustup installs it on the first build"; fi
target="${CARGO_TARGET_DIR:-$HOME/.cache/holochain-wifi-direct-demo/target}"
mkdir -p "$target" 2> /dev/null
free_gb="$(df -BG --output=avail "$target" 2> /dev/null | tail -1 | tr -dc 0-9)"
if [[ -n "$free_gb" && "$free_gb" -ge 8 ]]; then pass "$free_gb GB free for $target"; else warn "only ${free_gb:-?} GB free for $target; a release build needs about 4 GB, a debug one about 4 GB more"; fi

echo "== NetworkManager and Wi-Fi"
if ! command -v nmcli > /dev/null; then
  fail "nmcli missing; hc-wfd joins through NetworkManager"
else
  [[ "$(nmcli -t -f RUNNING general 2> /dev/null)" == "running" ]] && pass "NetworkManager running" || fail "NetworkManager is not running"
  [[ "$(nmcli radio wifi 2> /dev/null)" == "enabled" ]] && pass "Wi-Fi radio on" || fail "Wi-Fi is off: nmcli radio wifi on"
  iface="${1:-$(nmcli -t -f DEVICE,TYPE device | awk -F: '$2 == "wifi" {print $1; exit}')}"
  if [[ -z "$iface" ]]; then
    fail "no Wi-Fi interface managed by NetworkManager"
  else
    current="$(nmcli -t -f DEVICE,CONNECTION device | awk -F: -v d="$iface" '$1 == d {print $2}')"
    pass "Wi-Fi interface $iface${current:+ (now on '$current')}"
    [[ -n "$current" && "$current" != hc-wfd-* ]] && info "joining the group moves $iface off '$current' until hc-wfd ends; hc-wfd asks first"
    driver="$(basename "$(readlink -f "/sys/class/net/$iface/device/driver" 2> /dev/null)" 2> /dev/null)"
    [[ -n "$driver" ]] && info "driver $driver"
    if command -v iw > /dev/null; then
      phy="$(iw dev "$iface" info 2> /dev/null | awk '/wiphy/ {print "phy" $2}')"
      modes="$(iw phy "$phy" info 2> /dev/null | sed -n '/Supported interface modes/,/^\t[A-Z]/p' | grep -o '\* .*' | tr -d '* ' | tr '\n' ' ')"
      info "interface modes: $modes"
      grep -qw managed <<< "$modes" && pass "station (managed) mode, used to join as a legacy client" || fail "no managed mode"
      grep -qw P2P-client <<< "$modes" && info "P2P-client supported too (only needed for a future native P2P mode)"
      iw phy "$phy" info 2> /dev/null | sed -n '/valid interface combinations/,/^\t[A-Z]/p' | sed 's/^/      /' | head -8
    else
      warn "iw not installed (sudo apt install iw); hc-wfd reports the group's BSSID with it"
    fi
  fi
fi

echo "== Firewall"
if [[ -r /etc/ufw/ufw.conf ]] && grep -q "ENABLED=yes" /etc/ufw/ufw.conf; then
  warn "ufw is enabled: after joining, open the Holochain UDP port on the Wi-Fi interface with scripts/ubuntu-firewall.sh open"
else
  pass "ufw is not enabled"
fi

echo "== Optional tools"
command -v zbarcam > /dev/null && pass "zbarcam, for hc-wfd join --camera" || info "zbarcam not installed; --camera needs: sudo apt install zbar-tools"
command -v tcpdump > /dev/null && pass "tcpdump, for the packet capture" || info "tcpdump not installed; scripts/ubuntu-verify-link.sh's capture needs it"

echo
if (( failures > 0 )); then echo "RESULT  FAIL ($failures failed)"; exit 1; fi
echo "RESULT  PASS"
