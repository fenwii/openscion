#!/usr/bin/env python3
"""Stdlib-only structural checks; does not replace cargo build or test."""
from pathlib import Path
import tomllib
root=Path(__file__).resolve().parents[1]
workspace=tomllib.loads((root/'Cargo.toml').read_text())
errors=[]
for member in workspace['workspace']['members']:
    cargo=root/member/'Cargo.toml'
    if not cargo.exists(): errors.append(f'missing manifest: {cargo}')
    else:
        parsed=tomllib.loads(cargo.read_text())
        if 'package' not in parsed: errors.append(f'missing package section in {member}')
        src=root/member/'src'/'main.rs' if member.endswith('-cli') else root/member/'src'/'lib.rs'
        if not src.exists(): errors.append(f'missing crate root: {src}')
for file in ['README.md','LICENSE','docs/ARCHITECTURE.md','docs/OBJECT_FORMAT.md','docs/TESTING.md','scripts/smoke.sh','.github/workflows/ci.yml']:
    if not (root/file).exists(): errors.append(f'missing required file: {file}')
if errors:
    print('\n'.join(errors))
    raise SystemExit(1)
print(f'STRUCTURE OK: {len(workspace["workspace"]["members"])} Rust crates, required docs and CI present')
print('CARGO BUILD NOT EXECUTED by this script')
