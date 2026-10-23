#!/usr/bin/env bash
# Link-outage regression test: two hc-wfd nodes keep exchanging notes while
# first the joiner's, then the host's link address disappears for a while and
# comes back, as a Wi-Fi Direct address does when the link drops.
#
# THIS IS NOT A WI-FI DIRECT TEST. Each node runs in its own private network
# namespace (unshare, no sudo needed), the two joined by a virtual Ethernet
# pair, so like two machines they share nothing but that link: not even
# loopback. Nothing on the real network changes. It checks that Holochain,
# kitsune2 and iroh recover from a link address going away, which hardware
# test I then confirms over the radio.
#
# Usage: scripts/outage-test.sh [OUTAGE_SECONDS]   (default 45)
# Environment: BIN (hc-wfd), RUN_DIR (short path for node data), TIMEOUT_S
# (default 900), CAPTURE=1 to save the link's packets to RUN_DIR/link.pcap.
set -uo pipefail
cd "$(dirname "$0")/.."
outage="${1:-45}"

if [[ -z "${HCWFD_IN_NETNS:-}" ]]; then
  exec unshare -rn env HCWFD_IN_NETNS=1 "$0" "$@"
fi

target="${CARGO_TARGET_DIR:-$HOME/.cache/holochain-wifi-direct-demo/target}"
BIN="${BIN:-$target/release/hc-wfd}"
[[ -x "$BIN" ]] || { echo "hc-wfd not found at $BIN" >&2; exit 2; }
dir="${RUN_DIR:-$HOME/hcw/hcwfd-outage}"
rm -rf "$dir"; mkdir -p "$dir"

pids=()
trap 'for p in "${pids[@]}"; do kill "$p" 2> /dev/null; done' EXIT

# This namespace is the host's; a second one, held open by a sleeping
# process, is the joiner's.
unshare -n sleep infinity &
ns2=$!; pids+=("$ns2")
sleep 0.5
in_ns2() { nsenter -t "$ns2" -n "$@"; }
ip link set lo up
ip link add va type veth peer name vb
ip link set vb netns "$ns2"
ip addr add 10.77.0.1/24 dev va
ip link set va up
in_ns2 ip link set lo up
in_ns2 ip addr add 10.77.0.2/24 dev vb
in_ns2 ip link set vb up
notes=(--notes 10 --note-bytes 20000 --note-interval-s 15 --timeout-s "${TIMEOUT_S:-900}" --linger-s 60)
if [[ -n "${CAPTURE:-}" ]]; then
  tcpdump -i va -n -w "$dir/link.pcap" udp 2> /dev/null &
  pids+=($!)
fi

"$BIN" create --no-wifi --link-ip 10.77.0.1 --no-open-qr --qr-out "$dir/invite.png" \
  --data-dir "$dir" "${notes[@]}" > "$dir/host.out" 2> "$dir/host.err" &
pids+=($!); host=$!
until [[ -s "$dir/invite.png" ]]; do sleep 1; done
in_ns2 "$BIN" join --no-wifi --link-ip 10.77.0.2 --qr-image "$dir/invite.png" \
  --data-dir "$dir" "${notes[@]}" > "$dir/join.out" 2> "$dir/join.err" &
pids+=($!); joiner=$!

until grep -q "Peer Connected" "$dir/join.out" 2> /dev/null; do
  kill -0 "$joiner" 2> /dev/null || { echo "the joiner stopped before connecting"; exit 1; }
  sleep 1
done
echo "connected; first outage in 10s"
sleep 10

outage_of() { # name address interface [command prefix]
  local name="$1" addr="$2" dev="$3"; shift 3
  echo "$(date +%T) $name's address $addr goes away for ${outage}s"
  "$@" ip addr del "$addr/24" dev "$dev"
  local end=$((SECONDS + outage))
  # Other addresses come and go meanwhile, as they do while Windows tears a
  # group adapter down; each change makes iroh try to rebind again.
  while (( SECONDS < end )); do
    "$@" ip addr add 10.78.0.9/24 dev "$dev"; sleep 1
    "$@" ip addr del 10.78.0.9/24 dev "$dev"; sleep 2
  done
  "$@" ip addr add "$addr/24" dev "$dev"
  echo "$(date +%T) $name's address $addr is back"
}
outage_of joiner 10.77.0.2 vb in_ns2
sleep 20
outage_of host 10.77.0.1 va

wait "$host"; host_rc=$?
wait "$joiner"; join_rc=$?
for side in host join; do
  grep -E "^read|All transports|RESULT| -> " "$dir/$side.out" | sed "s/^/$side: /"
  grep -h "All transports failed" "$dir/$side.err" | sed "s/^/$side log: /" | head -2
done
if [[ $host_rc == 0 && $join_rc == 0 ]] &&
   [[ "$(grep -c ' PASS  10 note' "$dir/host.out" "$dir/join.out" | awk -F: '{s += $2} END {print s}')" == 4 ]]; then
  echo "RESULT  PASS (simulated link outages between two network namespaces; NOT Wi-Fi Direct)"
else
  echo "RESULT  FAIL (host $host_rc, joiner $join_rc)"
  exit 1
fi
