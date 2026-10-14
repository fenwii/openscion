#!/usr/bin/env python3
from pathlib import Path
import tomllib,sys
root=Path(__file__).resolve().parents[1]
w=tomllib.loads((root/'Cargo.toml').read_text())
for member in w['workspace']['members']:
    d=root/member
    assert (d/'Cargo.toml').is_file(),member
    tomllib.loads((d/'Cargo.toml').read_text())
    assert (d/'src/lib.rs').is_file() or (d/'src/main.rs').is_file(),member
for f in (root/'sdk/typescript/package.json',root/'sdk/typescript/tsconfig.json'):
    import json
    json.loads(f.read_text())
print('PASS: Cargo manifests, crate scaffolding, SDK JSON structure')
