BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS change_grants (
  principal_id TEXT NOT NULL REFERENCES principals(id),
  repo_id TEXT NOT NULL REFERENCES git_repositories(id),
  permission TEXT NOT NULL,
  PRIMARY KEY(principal_id, repo_id, permission)
);
CREATE TABLE IF NOT EXISTS change_policies (
  repo_id TEXT NOT NULL REFERENCES git_repositories(id),
  target_ref TEXT NOT NULL,
  policy_json TEXT NOT NULL,
  policy_digest TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(repo_id,target_ref)
);
CREATE TABLE IF NOT EXISTS change_requests (
  id TEXT PRIMARY KEY,
  repo_id TEXT NOT NULL REFERENCES git_repositories(id),
  source_workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  source_oid TEXT NOT NULL,
  base_oid TEXT NOT NULL,
  target_ref TEXT NOT NULL,
  author TEXT NOT NULL REFERENCES principals(id),
  title TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  idem_key TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  state TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  generation INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(author,repo_id,idem_key)
);
CREATE INDEX IF NOT EXISTS change_by_repo ON change_requests(repo_id,created_at);
CREATE TABLE IF NOT EXISTS land_operations (
  id TEXT PRIMARY KEY,
  change_id TEXT NOT NULL REFERENCES change_requests(id),
  actor TEXT NOT NULL REFERENCES principals(id),
  idem_key TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  state TEXT NOT NULL,
  attempt INTEGER NOT NULL DEFAULT 0,
  result_oid TEXT,
  error_code TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(change_id,actor,idem_key)
);
CREATE TABLE IF NOT EXISTS land_attempts (
  operation_id TEXT NOT NULL REFERENCES land_operations(id),
  attempt INTEGER NOT NULL,
  observed_target TEXT NOT NULL,
  source_oid TEXT NOT NULL,
  candidate_tree TEXT,
  candidate_commit TEXT,
  candidate_ref TEXT,
  policy_digest TEXT NOT NULL,
  state TEXT NOT NULL,
  PRIMARY KEY(operation_id,attempt)
);
CREATE TABLE IF NOT EXISTS validation_runs (
  id TEXT PRIMARY KEY,
  operation_id TEXT NOT NULL REFERENCES land_operations(id),
  attempt INTEGER NOT NULL,
  candidate_oid TEXT NOT NULL,
  tree_oid TEXT NOT NULL,
  check_name TEXT NOT NULL,
  suite_digest TEXT NOT NULL,
  runner_digest TEXT NOT NULL,
  policy_digest TEXT NOT NULL,
  principal_id TEXT,
  state TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(operation_id,attempt,check_name)
);
CREATE TABLE IF NOT EXISTS review_decisions (
  id TEXT PRIMARY KEY,
  change_id TEXT NOT NULL REFERENCES change_requests(id),
  operation_id TEXT NOT NULL REFERENCES land_operations(id),
  attempt INTEGER NOT NULL,
  candidate_oid TEXT NOT NULL,
  tree_oid TEXT NOT NULL,
  policy_digest TEXT NOT NULL,
  reviewer TEXT NOT NULL REFERENCES principals(id),
  decision TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(operation_id,attempt,reviewer)
);
CREATE TABLE IF NOT EXISTS change_outbox (
  id TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  delivered_at INTEGER
);
PRAGMA user_version=4;
COMMIT;
