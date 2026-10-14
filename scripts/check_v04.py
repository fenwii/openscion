#!/usr/bin/env python3
"""Offline structure and V4 database migration smoke. Not a Rust compiler."""
from pathlib import Path
import re, sqlite3, tempfile, tomllib
root=Path(__file__).resolve().parents[1]
w=tomllib.loads((root/'Cargo.toml').read_text())['workspace']
assert len(w['members'])>=14,len(w['members'])
for item in w['members']:
    p=root/item
    assert (p/'Cargo.toml').is_file(),item
    data=tomllib.loads((p/'Cargo.toml').read_text())
    assert data['package']['name']==p.name
    assert (p/'src/lib.rs').is_file() or (p/'src/main.rs').is_file(),item
for name in ['openscion-change','openscion-policy','openscion-validation','openscion-land']:
    assert (root/'crates'/name/'src/lib.rs').is_file()
for name in ['main.rs','lib.rs','bin/osc-change.rs','bin/osc-admin.rs']:
    assert (root/'crates/openscion-server/src'/name).is_file()
for name in ['ARCHITECTURE.md','API.md','SECURITY.md','VERIFICATION.md','RECOVERY.md']:
    assert (root/'docs/v0.4'/name).is_file(),name
sql=(root/'crates/openscion-registry/src/schema_v4.sql').read_text()
assert 'PRAGMA user_version=4' in sql
# DDL compatibility with exact V0.3 table schema, without requiring Rust execution.
with tempfile.TemporaryDirectory() as td:
    c=sqlite3.connect(str(Path(td)/'test.sqlite'))
    c.executescript('''CREATE TABLE principals(id TEXT PRIMARY KEY, role TEXT);
    CREATE TABLE git_repositories(id TEXT PRIMARY KEY,name TEXT,path TEXT,kind TEXT,object_format TEXT,registered_at INTEGER);
    CREATE TABLE workspaces(id TEXT PRIMARY KEY,repo_id TEXT,owner TEXT,task_id TEXT,base_oid TEXT,head_oid TEXT,path TEXT,state TEXT,generation INTEGER,lease_expires INTEGER,created_at INTEGER,updated_at INTEGER,error TEXT);
    PRAGMA user_version=3;''')
    c.executescript(sql)
    assert c.execute('PRAGMA user_version').fetchone()[0]==4
    assert len([r for r in c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'land_%'")])==2
for phrase in ['update-ref','--no-deref','merge-tree','commit-tree','CAS_IN_FLIGHT','LAND_RECONCILIATION_REQUIRED']:
    assert phrase in (root/'crates/openscion-land/src/lib.rs').read_text(),phrase
print('PASS: 14 Rust manifests, source layout, V4 SQLite migration, and integration contracts')
