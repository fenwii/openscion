#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ -x target/release/osc ]]; then OSC="$PWD/target/release/osc"; else OSC="$PWD/target/debug/osc"; fi
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/source/nested"
printf 'hello world\n' > "$TMP/source/nested/README.md"
"$OSC" init "$TMP/repo"
"$OSC" snapshot create --repo "$TMP/repo" --from "$TMP/source" --name baseline
printf 'new version\n' > "$TMP/source/nested/README.md"
"$OSC" snapshot create --repo "$TMP/repo" --from "$TMP/source" --name update
"$OSC" fsck --repo "$TMP/repo" --all
"$OSC" snapshot export baseline --repo "$TMP/repo" --to "$TMP/restored"
cmp "$TMP/restored/nested/README.md" <(printf 'hello world\n')
printf 'OpenScion V0.1 CLI smoke test PASSED\n'
