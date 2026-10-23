#!/usr/bin/env bash
# Open, and later close, the Holochain UDP port in ufw for the Wi-Fi Direct
# link only.
#
# Usage:
#   scripts/ubuntu-firewall.sh open [UDP_PORT]    (default 47001)
#   scripts/ubuntu-firewall.sh close
#
# `open` finds the active hc-wfd-* NetworkManager profile, and allows
# UDP_PORT only on its interface, only to this machine's address there, and
# only from that subnet. `close` deletes exactly the rules `open` added. Both
# need sudo. Not needed when ufw is off.
#
# Only inbound UDP needs a rule: Ubuntu dials the pairing service, so its TCP
# is outbound. Windows may also dial Ubuntu's Holochain endpoint, which is
# inbound UDP.
set -euo pipefail
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/hc-wfd/ufw-rules"
mkdir -p "$(dirname "$STATE")"
die() { echo "error: $*" >&2; exit 1; }

case "${1:-}" in
  open)
    port="${2:-47001}"
    command -v ufw > /dev/null || die "ufw is not installed; no rule needed"
    line="$(nmcli -t -f NAME,DEVICE connection show --active | grep '^hc-wfd-' | head -1)" ||
      die "no active hc-wfd-* profile: run hc-wfd join first"
    iface="${line#*:}"
    cidr="$(ip -4 -o addr show dev "$iface" | awk '{print $4; exit}')"
    [[ -n "$cidr" ]] || die "$iface has no IPv4 address"
    subnet="$(ip -4 route show dev "$iface" scope link | awk '{print $1; exit}')"
    args=(allow in on "$iface" from "$subnet" to "${cidr%/*}" port "$port" proto udp)
    sudo ufw "${args[@]}" comment "hc-wfd"
    printf '%s\n' "${args[*]}" >> "$STATE"
    echo "opened    UDP $port to ${cidr%/*} from $subnet on $iface; '$0 close' removes it"
    ;;
  close)
    [[ -s "$STATE" ]] || { echo "no rules recorded"; exit 0; }
    while read -r -a args; do sudo ufw delete "${args[@]}" || true; done < "$STATE"
    rm -f "$STATE"
    echo "removed   the hc-wfd ufw rules"
    ;;
  *)
    sed -n '2,16p' "$0"
    exit 2
    ;;
esac
