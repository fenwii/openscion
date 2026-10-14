# OpenScion V0.6 — Implementation Architecture

**Status:** source implementation, unqualified. **Source of truth:** Git by default.

## Rust workspace

The 17 V0.5 crates remain intact. V0.6 adds five crates:

| Crate | Responsibility | Important invariant |
|---|---|---|
| `openscion-authority` | Authority epochs, opt-in preflight, freeze, preview promotion, quarantine | No implicit native authority |
| `openscion-native-ref` | Bootstrap, verified closure, transactional CAS, idempotent ref journal/outbox | Mode, epoch, generation and target checked in one SQLite transaction |
| `openscion-gateway` | Verified-import-only Git mirror and three-phase fallback | Mirror never updates protected `refs/heads/*`; fallback does it only after freeze and CAS |
| `openscion-operations` | Offline SQLite + native CAS + Git-bundle backups, SHA-256 manifests, restore plan | No live multi-store atomicity claim |
| `openscion-release` | Release-evidence report and SQLite integrity doctor | Cannot mark commercial GA without G0–G7 evidence |

### Authority states

`GIT_AUTHORITY -> CUTOVER_FROZEN -> NATIVE_AUTHORITY_PREVIEW -> FALLBACK_FROZEN -> GIT_AUTHORITY`.

`QUARANTINED` is a protective, no-writes state. `SHADOW_QUALIFYING`, `CUTOVER_PREPARING`, `FALLBACK_PREPARING`, and `READ_ONLY` remain reserved in the schema for subsequent operational integrations.

### Git writer fencing

A shared per-repository `land-<repo-id>.lock` serializes the V0.4 protected landing operation with the V0.6 freeze transition. Both operations check the authority mode after obtaining that lock. V0.3 workspace creation and commits separately check authority mode, but OS-level permissions must also fence external writers. Never assume a SQLite mode flag prevents direct filesystem writes by unrelated processes.

### Native Ref transaction

- `ChangesetStore::get` checks the object hash and canonical encoding.
- Every reachable parent is traversed within a 10,000-Changeset bound.
- Native Blobs are read and hash-verified; the mapped root Tree is traversed and verified.
- The target must have a V0.5 `VERIFIED` mapping in this repository.
- The new Ref target must fast-forward from the supplied expected native Changeset.
- SQLite `BEGIN IMMEDIATE` checks authority mode and epoch in the same transaction as target + generation CAS.
- Idempotency request fingerprint and Ref journal are written in the transaction.
- An outbox event is committed in the same transaction.

No new `OSCCSET` format is invented. Because V0.5 Changesets carry Git provenance, V0.6 allows reference movement only among already verified imported Changesets.

### One-way Git mirror

`native refs/heads/main -> verified Git/native mapping -> native/Git semantic parity -> refs/openscion/mirror/main`.

The mirror reports native and mirrored generations. It does not support arbitrary newly authored native Changeset export. The Git mirror OID is the original Git commit OID, not a claim that native Changeset IDs equal Git OIDs.

### Fallback

1. Check exact native Ref and mirror generation, and verify parity.
2. Atomically change authority to `FALLBACK_FROZEN`, increment epoch, and retain checkpoint.
3. CAS the Git `refs/heads/main` from the recorded cutover Git checkpoint to the verified mirror OID (or detect already applied).
4. Commit authority to `GIT_AUTHORITY` with a fresh epoch and append audit transition.

A crash after phase 2 can be reconciled by reissuing fallback using the new frozen epoch. All API writes are blocked while `FALLBACK_FROZEN`.

### Storage and deployment limits

SQLite is single-site authority metadata. CAS bytes and SQLite are not one transaction; immutable object publication precedes Ref mutation. Production multi-host writes require a separately proven fencing/consensus design. No native REST API should attempt to silently redirect to Git mode.

See `../V06_COMMERCIAL_ARCHITECTURE.md` for the comprehensive commercial architecture baseline.
