#!/usr/bin/env bash
# Show, independently of hc-wfd's own report, that Holochain traffic to the
# Windows peer crosses the Wi-Fi Direct link.
#
# Usage: scripts/ubuntu-verify-link.sh PEER_IP [UDP_PORT [SECONDS]]
#
# Run it while `hc-wfd join` is exchanging notes. It checks the route the
# kernel picks for PEER_IP (the Wi-Fi Direct interface, no gateway), the
# Wi-Fi association (a DIRECT- SSID through an hc-wfd-* profile), that
# hc-wfd's only UDP socket is bound on that link, and then captures UDP_PORT
# (default 47001) on every interface for SECONDS (default 30, needs sudo) and
# counts packets per interface. Every packet should be on the Wi-Fi Direct
# interface. Capture lines are saved to wfd-capture-<time>.txt here; they
# hold the two link addresses and packet sizes, nothing else.
set -uo pipefail
peer="${1:?usage: ubuntu-verify-link.sh PEER_IP [UDP_PORT [SECONDS]]}"
port="${2:-47001}"
seconds="${3:-30}"
failures=0
pass() { echo "PASS  $*"; }
warn() { echo "WARN  $*"; }
fail() { echo "FAIL  $*"; failures=$((failures + 1)); }

echo "== Route to $peer"
route="$(ip route get "$peer" 2> /dev/null | head -1)"
dev="$(awk '{for (i = 1; i < NF; i++) if ($i == "dev") {print $(i + 1); exit}}' <<< "$route")"
src="$(awk '{for (i = 1; i < NF; i++) if ($i == "src") {print $(i + 1); exit}}' <<< "$route")"
echo "      $route"
if [[ -z "$dev" ]]; then
  fail "no route to $peer"
elif grep -q " via " <<< "$route"; then
  fail "$peer is reached through a gateway, not directly on a link"
elif [[ -d "/sys/class/net/$dev/wireless" || -d "/sys/class/net/$dev/phy80211" ]]; then
  pass "$peer is on the link of Wi-Fi interface $dev, from $src"
else
  fail "$peer is reached through $dev, which is not a Wi-Fi interface"
fi

if [[ -n "$dev" ]]; then
  echo "== Wi-Fi association on $dev"
  link="$(iw dev "$dev" link 2> /dev/null)"
  if grep -q "Connected to" <<< "$link"; then
    grep -E "Connected to|SSID|freq|signal|tx bitrate" <<< "$link" | sed 's/^\s*/      /'
    connection="$(nmcli -t -f DEVICE,CONNECTION device 2> /dev/null | awk -F: -v d="$dev" '$1 == d {print $2}')"
    ssid="$(awk -F': ' '/SSID/ {print $2}' <<< "$link")"
    if [[ "$connection" == hc-wfd-* && "$ssid" == DIRECT-* ]]; then
      pass "$dev is on the Wi-Fi Direct group $ssid through profile $connection"
    else
      fail "$dev is on '$connection' (SSID '$ssid'), not an hc-wfd Wi-Fi Direct group: this is an ordinary Wi-Fi network"
    fi
  else
    fail "$dev is not associated"
  fi
fi

echo "== hc-wfd sockets"
sockets="$(ss -uanp 2> /dev/null | grep '"hc-wfd"' || true)"
if [[ -z "$sockets" ]]; then
  fail "no hc-wfd UDP socket found; is hc-wfd join running?"
else
  sed 's/^/      /' <<< "$sockets"
  if [[ "$(wc -l <<< "$sockets")" == 1 ]] && grep -q "$src:$port " <<< "$sockets"; then
    pass "hc-wfd's only UDP socket is bound to $src:$port on $dev"
  else
    fail "expected exactly one hc-wfd UDP socket, bound to $src:$port"
  fi
fi
tcp="$(ss -tanp 2> /dev/null | grep '"hc-wfd"' | awk '{print $1, $4, $5}' || true)"
[[ -n "$tcp" ]] && { echo "      TCP:"; sed 's/^/        /' <<< "$tcp"; }

echo "== Capture of UDP port $port on all interfaces for ${seconds}s"
if command -v tcpdump > /dev/null; then
  out="wfd-capture-$(date +%Y%m%d-%H%M%S).txt"
  sudo timeout "$seconds" tcpdump -i any -nn -l "udp port $port" 2> /dev/null > "$out"
  if [[ ! -s "$out" ]]; then
    fail "no packets on UDP port $port; is a Holochain exchange running?"
  else
    echo "      packets per interface:"
    awk '{print $2}' "$out" | sort | uniq -c | sed 's/^/        /'
    others="$(awk -v d="$dev" '$2 != d' "$out" | wc -l)"
    total="$(wc -l < "$out")"
    if (( others == 0 )); then
      pass "all $total packets on UDP port $port crossed $dev (saved to $out)"
    else
      fail "$others of $total packets used another interface (saved to $out)"
    fi
  fi
else
  warn "tcpdump is not installed (sudo apt install tcpdump)"
fi

echo
if (( failures > 0 )); then echo "RESULT  FAIL ($failures failed)"; exit 1; fi
echo "RESULT  PASS"
