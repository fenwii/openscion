BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS repository_authority_v6 (
 repo_id TEXT PRIMARY KEY REFERENCES git_repositories(id),
 mode TEXT NOT NULL DEFAULT 'GIT_AUTHORITY' CHECK (mode IN ('GIT_AUTHORITY','SHADOW_QUALIFYING','CUTOVER_PREPARING','CUTOVER_FROZEN','NATIVE_AUTHORITY_PREVIEW','FALLBACK_PREPARING','FALLBACK_FROZEN','READ_ONLY','QUARANTINED')),
 epoch INTEGER NOT NULL DEFAULT 0 CHECK(epoch>=0),
 transition_id TEXT,
 checkpoint_git_oid TEXT,
 checkpoint_native_id TEXT,
 backup_id TEXT,
 updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS authority_transitions_v6 (
 id TEXT PRIMARY KEY,repo_id TEXT NOT NULL REFERENCES git_repositories(id),actor TEXT NOT NULL,
 from_mode TEXT NOT NULL,to_mode TEXT NOT NULL,from_epoch INTEGER NOT NULL,to_epoch INTEGER NOT NULL,
 git_checkpoint TEXT,native_checkpoint TEXT,backup_id TEXT,note TEXT NOT NULL,created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS native_refs_v6 (
 repo_id TEXT NOT NULL REFERENCES git_repositories(id),ref_name TEXT NOT NULL,
 target_changeset_id TEXT NOT NULL,generation INTEGER NOT NULL DEFAULT 0 CHECK(generation>=0),
 authority_epoch INTEGER NOT NULL CHECK(authority_epoch>=0),updated_at INTEGER NOT NULL,
 PRIMARY KEY(repo_id,ref_name)
);
CREATE TABLE IF NOT EXISTS native_ref_updates_v6 (
 id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,ref_name TEXT NOT NULL,actor TEXT NOT NULL,
 request_fingerprint TEXT NOT NULL,expected_target TEXT NOT NULL,expected_generation INTEGER NOT NULL,
 target_changeset_id TEXT NOT NULL,authority_epoch INTEGER NOT NULL,
 result_generation INTEGER NOT NULL,policy_evidence TEXT NOT NULL,created_at INTEGER NOT NULL,
 UNIQUE(repo_id,ref_name,id)
);
CREATE TABLE IF NOT EXISTS git_export_mappings_v6 (
 repo_id TEXT NOT NULL,native_id TEXT NOT NULL,git_oid TEXT NOT NULL,exporter_version INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('VERIFIED','BLOCKED','MISMATCH')),
 verified_at INTEGER NOT NULL,PRIMARY KEY(repo_id,native_id,exporter_version)
);
CREATE TABLE IF NOT EXISTS git_mirror_observations_v6 (
 repo_id TEXT NOT NULL,ref_name TEXT NOT NULL,native_id TEXT NOT NULL,native_generation INTEGER NOT NULL,
 git_oid TEXT NOT NULL,mirrored_generation INTEGER NOT NULL,verified_at INTEGER NOT NULL,
 PRIMARY KEY(repo_id,ref_name)
);
CREATE TABLE IF NOT EXISTS native_export_jobs_v6 (
 id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,native_id TEXT NOT NULL,state TEXT NOT NULL,
 generation INTEGER NOT NULL DEFAULT 0,lease_until INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,
 last_error TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS repository_qualifications_v6 (
 id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,git_oid TEXT NOT NULL,native_id TEXT NOT NULL,
 native_root TEXT NOT NULL,checks_json TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS backup_catalog_v6 (
 id TEXT PRIMARY KEY,root_path TEXT NOT NULL,manifest_sha256 TEXT NOT NULL,
 status TEXT NOT NULL,created_at INTEGER NOT NULL,verified_at INTEGER
);
CREATE TABLE IF NOT EXISTS operational_incidents_v6 (
 id TEXT PRIMARY KEY,repo_id TEXT,code TEXT NOT NULL,description TEXT NOT NULL,
 state TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS release_capabilities_v6 (
 key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS authority_transitions_repo_v6 ON authority_transitions_v6(repo_id,created_at);
CREATE INDEX IF NOT EXISTS native_refs_repo_v6 ON native_refs_v6(repo_id);
INSERT OR IGNORE INTO release_capabilities_v6(key,value,updated_at) VALUES('native_authority_preview','disabled',0);
INSERT OR IGNORE INTO release_capabilities_v6(key,value,updated_at) VALUES('commercial_ga_verified','false',0);
INSERT OR IGNORE INTO repository_authority_v6(repo_id,mode,epoch,updated_at)
 SELECT id,'GIT_AUTHORITY',0,CAST(strftime('%s','now') AS INTEGER) FROM git_repositories;
PRAGMA user_version=6;
COMMIT;
