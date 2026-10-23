#!/usr/bin/env bash
# Build the probe zomes and pack happ/probe.happ.
#
# happ/probe.happ is committed: it is holochain-ble-demo's ble_probe.happ,
# byte for byte, and hc-wfd embeds it. You only need this script to change
# the zomes. Both conductors must run the same bundle, but they need not be
# rebuilt on each machine: each pairing session sets its own network seed, so
# the DNA hash differs per session anyway. Needs `hc` 0.7 and the
# wasm32-unknown-unknown target.
set -euo pipefail
cd "$(dirname "$0")/.."

(
  cd dna
  RUSTFLAGS='--cfg getrandom_backend="custom" -C link-arg=--allow-undefined' \
    cargo build --release --target wasm32-unknown-unknown
)

mkdir -p dna/workdir/zomes/integrity dna/workdir/zomes/coordinator
cp dna/target/wasm32-unknown-unknown/release/probe_integrity.wasm dna/workdir/zomes/integrity/
cp dna/target/wasm32-unknown-unknown/release/probe.wasm dna/workdir/zomes/coordinator/

hc dna pack dna/workdir -o dna/workdir/ble_probe.dna
hc app pack happ/workdir -o happ/probe.happ
echo "Built happ/probe.happ"
