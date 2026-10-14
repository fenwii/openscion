BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS shadow_repositories (
  repo_id TEXT PRIMARY KEY REFERENCES git_repositories(id),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
  tracked_ref TEXT NOT NULL DEFAULT 'refs/heads/main',
  state TEXT NOT NULL DEFAULT 'DISABLED',
  last_observed_oid TEXT,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS shadow_mappings (
  repo_id TEXT NOT NULL REFERENCES git_repositories(id),
  git_oid TEXT NOT NULL,
  native_id TEXT NOT NULL,
  root_tree_id TEXT NOT NULL,
  git_tree_oid TEXT NOT NULL,
  manifest_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('VERIFIED','MISMATCH','INELIGIBLE','BLOCKED')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(repo_id,git_oid)
);
CREATE INDEX IF NOT EXISTS shadow_mapping_native ON shadow_mappings(native_id);
CREATE TABLE IF NOT EXISTS shadow_git_blobs (
  repo_id TEXT NOT NULL,
  git_oid TEXT NOT NULL,
  native_blob_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(repo_id,git_oid)
);
CREATE TABLE IF NOT EXISTS shadow_derived (
  repo_id TEXT NOT NULL,
  native_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  algorithm_version INTEGER NOT NULL,
  result_id TEXT NOT NULL,
  value_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(repo_id,native_id,kind,algorithm_version)
);
CREATE TABLE IF NOT EXISTS shadow_jobs (
  id TEXT PRIMARY KEY,
  repo_id TEXT NOT NULL REFERENCES git_repositories(id),
  git_oid TEXT NOT NULL,
  job_kind TEXT NOT NULL CHECK(job_kind IN ('IMPORT')),
  state TEXT NOT NULL CHECK(state IN ('PENDING','RUNNING','VERIFIED','FAILED','INELIGIBLE')),
  generation INTEGER NOT NULL DEFAULT 0,
  lease_until INTEGER NOT NULL DEFAULT 0,
  worker TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(repo_id,git_oid,job_kind)
);
CREATE INDEX IF NOT EXISTS shadow_job_queue ON shadow_jobs(state,lease_until,created_at);
CREATE TABLE IF NOT EXISTS shadow_verifications (
  id TEXT PRIMARY KEY,
  repo_id TEXT NOT NULL,
  git_oid TEXT NOT NULL,
  native_id TEXT,
  algorithm_version INTEGER NOT NULL,
  state TEXT NOT NULL,
  detail TEXT,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS shadow_ref_observations (
  id TEXT PRIMARY KEY,
  repo_id TEXT NOT NULL,
  ref_name TEXT NOT NULL,
  git_oid TEXT NOT NULL,
  observed_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS shadow_pins (
  repo_id TEXT NOT NULL,
  native_id TEXT NOT NULL,
  reason TEXT NOT NULL,
  expires_at INTEGER,
  PRIMARY KEY(repo_id,native_id,reason)
);
PRAGMA user_version=5;
COMMIT;
