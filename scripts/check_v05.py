#!/usr/bin/env python3
"""Offline V0.5 structural, schema, and interface check (NOT a Rust build)."""
from pathlib import Path
import sqlite3, tempfile, tomllib
root=Path(__file__).resolve().parents[1]
w=tomllib.loads((root/'Cargo.toml').read_text())['workspace']
assert len(w['members'])>=17,len(w['members'])
for crate in w['members']:
    p=root/crate
    assert (p/'Cargo.toml').is_file()
    assert tomllib.loads((p/'Cargo.toml').read_text())['package']['name']==p.name
    assert (p/'src/lib.rs').is_file() or (p/'src/main.rs').is_file()
for crate in ('openscion-native','openscion-derived','openscion-shadow'):
    assert (root/'crates'/crate/'src/lib.rs').is_file()
src=(root/'crates/openscion-shadow/src/lib.rs').read_text()
for marker in ('import_history','derive_root','verify_parity','shadow_jobs','SHADOW_MAPPING_MISMATCH','claim(', 'SHADOW_JOB_FENCE_LOST'):
    assert marker in src,marker
svc=(root/'crates/openscion-server/src/lib.rs').read_text()
for marker in ('shadow_enable','shadow_work','shadow_verify','shadow_mapping','shadow_jobs'):
    assert marker in svc,marker
with tempfile.TemporaryDirectory() as td:
    conn=sqlite3.connect(Path(td)/'schema.sqlite')
    conn.execute('PRAGMA foreign_keys=ON')
    conn.executescript('''
        CREATE TABLE principals(id TEXT PRIMARY KEY,role TEXT);
        CREATE TABLE git_repositories(id TEXT PRIMARY KEY,name TEXT,path TEXT,kind TEXT,object_format TEXT,registered_at INTEGER);
        CREATE TABLE workspaces(id TEXT PRIMARY KEY,repo_id TEXT,owner TEXT,task_id TEXT,base_oid TEXT,head_oid TEXT,path TEXT,state TEXT,generation INTEGER,lease_expires INTEGER,created_at INTEGER,updated_at INTEGER,error TEXT);
        PRAGMA user_version=3;
    ''')
    conn.executescript((root/'crates/openscion-registry/src/schema_v4.sql').read_text())
    assert conn.execute('PRAGMA user_version').fetchone()[0]==4
    conn.execute("INSERT INTO principals(id,role) VALUES ('admin','admin')")
    conn.execute("INSERT INTO git_repositories(id,name,path,kind,object_format,registered_at) VALUES ('r','repo','/tmp/test','normal','sha1',0)")
    conn.executescript((root/'crates/openscion-registry/src/schema_v5.sql').read_text())
    assert conn.execute('PRAGMA user_version').fetchone()[0]==5
    assert conn.execute('SELECT name FROM git_repositories WHERE id=?',['r']).fetchone()[0]=='repo'
    conn.execute("INSERT INTO shadow_repositories(repo_id,enabled,state,updated_at) VALUES ('r',1,'ENABLED',1)")
    conn.execute("INSERT INTO shadow_jobs(id,repo_id,git_oid,job_kind,state,created_at,updated_at) VALUES ('j','r','git:sha1:abcd','IMPORT','PENDING',1,1)")
    assert conn.execute('SELECT COUNT(*) FROM shadow_jobs').fetchone()[0]==1
for required in ('API.md','ARCHITECTURE.md','SECURITY.md','RECOVERY.md','VERIFICATION.md','OBJECT_FORMAT.md'):
    assert (root/'docs/v0.5'/required).is_file(),required
print('PASS: 17 Rust crates, V5 schema migration, shadow source contracts, and docs')
