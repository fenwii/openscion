# V0.6 HTTP and CLI Contract

All HTTP routes require a bearer token except liveness/readiness. Source APIs remain `/v1` additive. Repo IDs are opaque. No arbitrary host paths are accepted in normal source requests.

| Method | Path | Authorization |
|---|---|---|
| GET | `/v1/repos/{id}/authority` | Authorized repository reader |
| GET | `/v1/repos/{id}/native/refs?ref_name=refs/heads/main` | Authorized reader |
| POST | `/v1/repos/{id}/native/refs/cas` | Administrator, native preview + current epoch |
| GET | `/v1/repos/{id}/git-mirror/status?ref_name=refs/heads/main` | Authorized reader |
| POST | `/v1/repos/{id}/git-mirror/sync` | Administrator, native preview |

Native Ref CAS body:

```json
{
  "repo_id":"<repo-id>", "ref_name":"refs/heads/main",
  "expected_target":"osc:changeset:sha256:<64 hex>",
  "expected_generation":0,"authority_epoch":2,
  "new_target":"osc:changeset:sha256:<64 hex>",
  "idempotency_key":"task-1001-native-move",
  "policy_evidence":"validated-preview-checkpoint-001"
}
```

**Important:** The target must be a verified imported Changeset and a descendant of the expected target. All privileged authority transitions and backup operations are **local CLI only**.

`osc-ops --data <service-path> <COMMAND> [options]` exposes:

- `doctor`, `status`, `qualify`, `preflight`
- `feature-preview`, `freeze`, `bootstrap`, `promote`, `quarantine`
- `ref`, `cas`, `mirror`, `fallback`
- `backup`, `backup-verify`, `restore-plan`, `release-verify`

Examples:

```sh
osc-ops --data ./var/openscion status --repo <repo-id>
osc-ops --data ./var/openscion doctor
osc-ops --data ./var/openscion qualify --repo <repo-id>
osc-ops --data ./var/openscion backup --output ./backups --all-writers-stopped true
osc-ops --data ./var/openscion backup-verify --backup ./backups/backup-<id>
osc-ops --data ./var/openscion release-verify --evidence ./release-evidence.json
```

Native authority remains disabled by default; privileged Git cutover commands require the operator's independent write-fencing assertion. API errors have stable identifiers such as `AUTHORITY_FENCED`, `NATIVE_REF_CAS_CONFLICT`, `NATIVE_NON_FAST_FORWARD`, `GIT_MIRROR_LAGGING`, and `ROLLBACK_BLOCKED_BY_UNEXPORTED_COMMITS`.

The TypeScript `AuthorityPreviewClient` provides read-only authority queries, native Ref read/CAS and Git mirror status/sync with explicit authorization.
