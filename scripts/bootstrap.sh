#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || { echo 'Error: install Rust and Cargo before running bootstrap' >&2; exit 1; }
cargo generate-lockfile
cargo fmt --all
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
bash scripts/smoke.sh
