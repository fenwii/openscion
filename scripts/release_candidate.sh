#!/usr/bin/env bash
# Produces a locally built unsigned candidate. It deliberately DOES NOT certify GA.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || { echo 'Rust/Cargo required' >&2; exit 2; }
command -v git >/dev/null || { echo 'Git required' >&2; exit 2; }
if [ ! -f Cargo.lock ]; then
  echo 'Cargo.lock must be generated, reviewed and committed before release.' >&2
  exit 2
fi
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked
for v in 02 03 04 05 06; do python3 "scripts/check_v${v}.py"; done
for v in 04 05 06; do python3 "scripts/test_git_contract_v${v}.py"; done
for v in 02 03 04 05 06; do python3 "scripts/v${v}-smoke.py"; done
cargo build --release --workspace --locked
mkdir -p dist
(cd target/release && sha256sum osc-server osc-admin osc-ops osc-shadow osc-workspace osc-change) > dist/SHA256SUMS.txt
printf 'Candidate compiled and tested locally. Signatures and GA approval not created.\n'
