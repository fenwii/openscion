#!/usr/bin/env python3
"""Offline structural validation; not a substitute for Rust compiler/tests."""
from pathlib import Path
import tomllib, re
root=Path(__file__).resolve().parents[1]
workspace=tomllib.loads((root/'Cargo.toml').read_text())['workspace']
for crate in workspace['members']:
    manifest=root/crate/'Cargo.toml'
    package=tomllib.loads(manifest.read_text())['package']
    assert package['name']==Path(crate).name
    assert (root/crate/'src/lib.rs').exists() or (root/crate/'src/main.rs').exists()
    assert (root/crate/'Cargo.toml').exists()
assert len(workspace['members'])>=10
for expected in ['openscion-workspace','openscion-agent']:
    assert (root/'crates'/expected/'src/lib.rs').exists()
for item in ['README.md','docs/v0.3/ARCHITECTURE.md','docs/v0.3/API.md','docs/v0.3/SECURITY.md','docs/v0.3/VERIFICATION.md','scripts/v03-smoke.py']:
    assert (root/item).exists(), item
s=(root/'crates/openscion-workspace/src/lib.rs').read_text()
for term in ['recover_pending_commit','refs/openscion/workspaces','WORKSPACE_DIRTY','worktree','workspace_ops','WORKSPACE_HEAD_CHANGED']:
    assert term in s,term
print('PASS: all ten crates and V0.3 source/documentation assets present')
