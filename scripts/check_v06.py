#!/usr/bin/env python3
"""Offline structural and real SQLite V5->V6 migration test; NOT a Rust build."""
import pathlib,sqlite3,tempfile,tomllib,json
root=pathlib.Path(__file__).resolve().parents[1]
w=tomllib.loads((root/'Cargo.toml').read_text())['workspace']
assert w['package']['version']=='0.6.0'
assert len(w['members'])==22,len(w['members'])
for member in w['members']:
    crate=root/member
    meta=tomllib.loads((crate/'Cargo.toml').read_text())
    assert meta['package']['name']==crate.name
    assert (crate/'src/lib.rs').is_file() or (crate/'src/main.rs').is_file()
    for dep,opts in meta.get('dependencies',{}).items():
        if isinstance(opts,dict) and 'path' in opts:
            assert (crate/opts['path']/'Cargo.toml').exists(),(crate,dep)
with tempfile.TemporaryDirectory() as tmp:
    db=sqlite3.connect(str(pathlib.Path(tmp)/'meta.db'))
    db.execute('PRAGMA foreign_keys=ON')
    db.executescript('''
       CREATE TABLE principals(id TEXT PRIMARY KEY,role TEXT);
       CREATE TABLE git_repositories(id TEXT PRIMARY KEY,name TEXT,path TEXT,kind TEXT,object_format TEXT,registered_at INTEGER);
       CREATE TABLE workspaces(id TEXT PRIMARY KEY,repo_id TEXT,owner TEXT,task_id TEXT,base_oid TEXT,head_oid TEXT,path TEXT,state TEXT,generation INTEGER,lease_expires INTEGER,created_at INTEGER,updated_at INTEGER,error TEXT);
       PRAGMA user_version=3;
    ''')
    db.executescript((root/'crates/openscion-registry/src/schema_v4.sql').read_text())
    db.execute("INSERT INTO principals VALUES ('admin','admin')")
    db.execute("INSERT INTO git_repositories VALUES ('aaaa-bbbb','demo','/tmp/demo','bare','sha1',1)")
    db.executescript((root/'crates/openscion-registry/src/schema_v5.sql').read_text())
    db.execute("INSERT INTO shadow_repositories(repo_id,enabled,state,updated_at) VALUES ('aaaa-bbbb',1,'ENABLED',1)")
    db.executescript((root/'crates/openscion-registry/src/schema_v6.sql').read_text())
    assert db.execute('PRAGMA user_version').fetchone()[0]==6
    assert db.execute("SELECT mode,epoch FROM repository_authority_v6 WHERE repo_id='aaaa-bbbb'").fetchone()==('GIT_AUTHORITY',0)
    assert db.execute('PRAGMA integrity_check').fetchone()[0]=='ok'
    assert not db.execute('PRAGMA foreign_key_check').fetchall()
    # Real SQLite conditional update and ABA generation behavior
    with db:
        db.execute("UPDATE repository_authority_v6 SET mode='NATIVE_AUTHORITY_PREVIEW',epoch=4 WHERE repo_id='aaaa-bbbb'")
        db.execute("INSERT INTO native_refs_v6 VALUES ('aaaa-bbbb','refs/heads/main','A',0,4,1)")
    n=db.execute("UPDATE native_refs_v6 SET target_changeset_id='B',generation=generation+1 WHERE repo_id='aaaa-bbbb' AND ref_name='refs/heads/main' AND target_changeset_id='A' AND generation=0 AND authority_epoch=4").rowcount
    assert n==1
    db.execute("UPDATE native_refs_v6 SET target_changeset_id='A',generation=generation+1 WHERE repo_id='aaaa-bbbb'")
    n=db.execute("UPDATE native_refs_v6 SET target_changeset_id='C',generation=generation+1 WHERE repo_id='aaaa-bbbb' AND target_changeset_id='A' AND generation=0 AND authority_epoch=4").rowcount
    assert n==0,'ABA stale generation erroneously accepted'
    assert db.execute('SELECT generation FROM native_refs_v6').fetchone()[0]==2
    assert db.execute('PRAGMA integrity_check').fetchone()[0]=='ok'
for name in ['ARCHITECTURE.md','API.md','SECURITY.md','RECOVERY.md','OPERATIONS.md','RELEASE_GATES.md']:
    assert (root/'docs/v0.6'/name).exists(),name
print('PASS: 22-crate workspace, dependency wiring, V3->V6 migration, native CAS generation/ABA SQLite contract, documentation')
