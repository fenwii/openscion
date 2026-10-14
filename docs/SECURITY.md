# V0.6 Security Model and Limitations

V0.6 is **not penetration tested or commercially qualified**. Treat the current implementation as a reviewable source candidate.

The main trust boundaries are authenticated API callers, registered Git repositories, native immutable storage, SQLite authority data, mirror/export operations, privileged operator commands, and externally executed Agent code. A bearer token alone is not a secure network boundary; use TLS, OS filesystem permissions, strict firewall rules and secret rotation.

**Enforced mechanisms:**

- Native preview is disabled in the SQLite capability table by default.
- Promotion, fallback, backup and OS fencing approvals are local operator commands; no remote promotion API.
- Native Ref writes require administrator role, matching authority epoch, expected target and generation, idempotency key and policy evidence identifier.
- Ref targets require a verified imported mapping and a reachable, hash-checked Changeset/Blob/Tree closure.
- Native Ref movement rejects rewinds and unrelated histories.
- Protected Git landing checks mode after acquiring the same advisory lock used by the authority freeze.
- Mirror sync writes only `refs/openscion/mirror/*`; never protected branches.
- Fallback freezes native writes before Git ref updates.
- Backup paths are checked for containment and reject symlinks while copying managed storage.

**Limits requiring engineering qualification:**

1. An administrator-provided `external_writers_fenced=true` is an assertion, not proven OS-level fencing. Configure repository permissions and disable external Git push/receive paths before cutover.
2. V0.4 trusted-runner attestations are authenticated **claims**. Real signed evidence and sandbox execution must be independently integrated and verified before regulated commercial use.
3. SQLite local locks are not distributed consensus. Do not run two authorities on different hosts.
4. The current Git compatibility gateway cannot export arbitrary native-only commits.
5. Backup requires stopped writers; no online multi-store point-in-time consistency is claimed.
6. Native Ref CAS verifies closure before the SQLite transaction. Administrators must guarantee that immutable storage is not concurrently deleted or replaced outside the service.
7. Build provenance, dependency review, SBOM, release signing, incident-response testing, and customer data isolation require additional qualification.

For suspected corruption or split brain: stop writers, isolate the service, capture logs and state, enter `QUARANTINED`, and do not silently reset protected refs.
