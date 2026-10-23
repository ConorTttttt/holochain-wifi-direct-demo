#!/usr/bin/env bash
# Regression tests of hc-wfd with both nodes on this machine.
#
# THESE ARE NOT WI-FI DIRECT TESTS. Both nodes run here, with --no-wifi, and
# talk over loopback (127.0.0.1 and 127.0.0.2). They check the QR invitation,
# the authenticated pairing, mutual peer registration, the relay-free direct
# iroh transport and the two-way DHT exchange before any radio is involved.
#
# Usage: scripts/loopback-test.sh [TEST ...]
#   pair       host shows a QR PNG; joiner imports it, pairs, both exchange
#              2 x 50 kB notes; both must print PASS for both directions, and
#              each node must own exactly one UDP socket, on its link address
#   reuse      a second joiner with a copy of the same, already used, QR code
#              must fail to pair
#   no-wfd     `create` without --no-wifi on Linux must refuse clearly, and
#              `join` for a group that is not in range must fail at the scan
#              without touching the current Wi-Fi connection
# With no TEST, runs all three. Prints PASS or FAIL per test, exits 1 on any FAIL.
#
# Environment: BIN (hc-wfd), RUN_DIR (short path: Unix socket paths are
# limited to 107 bytes).
set -uo pipefail
cd "$(dirname "$0")/.."

target="${CARGO_TARGET_DIR:-$HOME/.cache/holochain-wifi-direct-demo/target}"
BIN="${BIN:-$target/release/hc-wfd}"
[[ -x "$BIN" ]] || BIN="$target/debug/hc-wfd"
[[ -x "$BIN" ]] || { echo "hc-wfd not found; run scripts/ubuntu-build.sh or set BIN" >&2; exit 2; }
RUN_DIR="${RUN_DIR:-$HOME/hcw/hcwfd-loopback}"
tests=("$@")
(( ${#tests[@]} )) || tests=(pair reuse no-wfd)

failures=0
pids=()
# The copied invitation is used up, but still holds a (now useless) secret.
cleanup() { for pid in "${pids[@]}"; do kill "$pid" 2> /dev/null; done; rm -f "$RUN_DIR/used-invite.png"; }
trap cleanup EXIT

wait_for() { # seconds, then a condition to evaluate
  local deadline=$((SECONDS + $1)); shift
  until eval "$@"; do (( SECONDS < deadline )) || return 1; sleep 1; done
}

# The process owns exactly one UDP socket, bound to the given address.
check_udp() { # name pid addr
  local udp
  udp="$(ss -uanp 2> /dev/null | grep "pid=$2," | awk '{print $4}' | sort -u)"
  if [[ "$udp" == "$3" ]]; then
    echo "      $1 UDP sockets: $udp only"
  else
    echo "      $1 UDP sockets: [$(tr '\n' ' ' <<< "$udp")], expected $3 only"
    return 1
  fi
}

host_args=(create --no-wifi --link-ip 127.0.0.1 --port 47101 --pair-port 47102 --no-open-qr
  --print-payload --notes 2 --note-bytes 50000 --timeout-s 240 --linger-s 30)
join_args=(join --no-wifi --link-ip 127.0.0.2 --notes 2 --note-bytes 50000 --timeout-s 240
  --linger-s 30)

run_pair() {
  local dir="$RUN_DIR/pair"
  rm -rf "$dir"; mkdir -p "$dir"
  "$BIN" "${host_args[@]}" --data-dir "$dir" --qr-out "$dir/invite.png" > "$dir/host.out" 2> "$dir/host.err" &
  local host=$!; pids+=("$host")
  wait_for 120 "[[ -s '$dir/invite.png' ]]" || { echo "      the host never wrote its QR code"; tail -5 "$dir/host.out"; return 1; }
  # Kept for the reuse test; the host deletes its own copy once it pairs.
  cp "$dir/invite.png" "$RUN_DIR/used-invite.png"
  "$BIN" "${join_args[@]}" --port 47111 --pair-timeout-s 60 --data-dir "$dir" --qr-image "$dir/invite.png" > "$dir/join.out" 2> "$dir/join.err" &
  local joiner=$!; pids+=("$joiner")

  local ok=0
  wait_for 240 "grep -q '^==> Synchronizing' '$dir/join.out'" || { echo "      the joiner never paired"; ok=1; }
  if (( ok == 0 )); then
    sleep 3
    check_udp host "$host" 127.0.0.1:47101 || ok=1
    check_udp joiner "$joiner" 127.0.0.2:47111 || ok=1
    if ss -tlnp 2> /dev/null | grep "pid=$host," | grep -q ":47102"; then
      echo "      the host still listens on its pairing port after pairing"; ok=1
    fi
  fi
  wait "$host"; local host_rc=$?
  wait "$joiner"; local join_rc=$?
  for side in host join; do
    grep -E "^(==> (QR code recognized|Pairing with peer|Holochain connected|Ready)|    \*  Peer Connected|transport|RESULT| +[a-z]+ -> )" "$dir/$side.out" | sed "s/^/      $side: /"
  done
  [[ $host_rc == 0 && $join_rc == 0 ]] || { echo "      exit codes: host $host_rc, joiner $join_rc"; ok=1; }
  for side in host join; do
    [[ "$(grep -c ' PASS  ' "$dir/$side.out")" == 2 ]] || { echo "      $side: not both directions PASS"; ok=1; }
    grep -q "Peer Connected" "$dir/$side.out" || { echo "      $side: no Peer Connected"; ok=1; }
    grep -q "NOT a Wi-Fi Direct" "$dir/$side.out" || { echo "      $side: the result is not labelled as not Wi-Fi Direct"; ok=1; }
  done
  # The host printed its invitation (--print-payload); nothing else may
  # contain the passphrase or the pairing secret: no log, no joiner output.
  local secrets
  secrets="$(grep -o 'HCWFD1:[A-Za-z0-9_-]*' "$dir/host.out" | head -1 | python3 -c '
import base64, json, sys
body = sys.stdin.read().strip().split(":", 1)[1]
invite = json.loads(base64.urlsafe_b64decode(body + "=" * (-len(body) % 4)))
print(invite["wifi"]["passphrase"]); print(invite["pairing"]["secret"])')"
  if [[ -z "$secrets" ]]; then
    echo "      could not read the printed invitation"; ok=1
  elif grep -qF -f <(echo "$secrets") "$dir/host.err" "$dir/join.err" "$dir/join.out"; then
    echo "      a log or the joiner's output contains the passphrase or pairing secret"; ok=1
  else
    echo "      neither log nor the joiner's output contains the passphrase or pairing secret"
  fi
  [[ -e "$dir/invite.png" ]] && { echo "      the host left its QR image behind"; ok=1; }
  return $ok
}

run_reuse() {
  local dir="$RUN_DIR/reuse"
  rm -rf "$dir"; mkdir -p "$dir"
  [[ -s "$RUN_DIR/used-invite.png" ]] || { echo "      run the pair test first"; return 1; }
  "$BIN" "${join_args[@]}" --name intruder --port 47121 --pair-timeout-s 8 --data-dir "$dir" \
    --qr-image "$RUN_DIR/used-invite.png" > "$dir/join.out" 2> "$dir/join.err"
  local rc=$?
  grep -E "^(==>|FAILED|pairing)" "$dir/join.out" | sed 's/^/      /'
  [[ $rc != 0 ]] && grep -q "FAILED at 'Pairing with peer'" "$dir/join.out"
}

run_no_wfd() {
  local dir="$RUN_DIR/no-wfd" ok=0
  rm -rf "$dir"; mkdir -p "$dir"
  "$BIN" create --data-dir "$dir" --no-open-qr --linger-s 0 > "$dir/create.out" 2>&1
  local rc=$?
  grep -E "^FAILED" "$dir/create.out" | sed 's/^/      create: /'
  [[ $rc != 0 ]] && grep -q "needs Windows" "$dir/create.out" || { echo "      create did not refuse clearly (rc $rc)"; ok=1; }

  if [[ -s "$RUN_DIR/used-invite.png" ]] && command -v nmcli > /dev/null; then
    local before after
    before="$(nmcli -t -f DEVICE,CONNECTION device | grep ':' | sort)"
    "$BIN" join --data-dir "$dir" --qr-image "$RUN_DIR/used-invite.png" --scan-timeout-s 10 < /dev/null > "$dir/join.out" 2>&1
    rc=$?
    after="$(nmcli -t -f DEVICE,CONNECTION device | grep ':' | sort)"
    grep -E "^(==>|FAILED)" "$dir/join.out" | sed 's/^/      join: /'
    [[ $rc != 0 ]] && grep -q "FAILED at 'Joining Wi-Fi Direct'" "$dir/join.out" || { echo "      join did not fail at the scan (rc $rc)"; ok=1; }
    [[ "$before" == "$after" ]] || { echo "      the network connections changed:"; diff <(echo "$before") <(echo "$after"); ok=1; }
    nmcli -t -f NAME connection show | grep -q '^hc-wfd-' && { echo "      an hc-wfd profile was left behind"; ok=1; }
  fi
  return $ok
}

for test in "${tests[@]}"; do
  echo "== $test (loopback, not Wi-Fi Direct)"
  if "run_${test//-/_}"; then echo "PASS  $test"; else echo "FAIL  $test"; failures=$((failures + 1)); fi
done
echo
if (( failures > 0 )); then echo "RESULT  FAIL ($failures failed, loopback only)"; exit 1; fi
echo "RESULT  PASS (loopback only: no Wi-Fi Direct was involved)"
