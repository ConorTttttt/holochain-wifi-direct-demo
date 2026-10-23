#!/usr/bin/env bash
# Build hc-wfd on Ubuntu, exactly as Cargo.lock pins it.
#
# Usage: scripts/ubuntu-build.sh [--debug]
#
# The build output goes to its own directory, separate from the BLE
# project's, so neither overwrites the other:
#   CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/holochain-wifi-direct-demo/target}
# The binary is $CARGO_TARGET_DIR/release/hc-wfd. A release build needs about
# 4 GB there and takes 5 to 10 minutes the first time.
set -euo pipefail
cd "$(dirname "$0")/.."

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/holochain-wifi-direct-demo/target}"
case "$CARGO_TARGET_DIR" in
  *" "*) echo "CARGO_TARGET_DIR must not contain spaces (libsodium's build fails): $CARGO_TARGET_DIR" >&2; exit 2 ;;
esac

profile=(--release)
dir=release
if [[ "${1:-}" == "--debug" ]]; then profile=(); dir=debug; fi

cargo build "${profile[@]}" --locked -p hc-wfd
bin="$CARGO_TARGET_DIR/$dir/hc-wfd"
"$bin" --version
echo "Built $bin"
