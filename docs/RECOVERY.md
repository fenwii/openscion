# V0.6 Authority and Recovery Protocols

## Git to native preview

Preflight re-verifies mapped Git tip and history. A local qualification record and **verified offline backup record** are required. The operator fences external Git writers, acquires the shared landing lock, and transitions from `GIT_AUTHORITY` to `CUTOVER_FROZEN` with epoch increment. Native bootstrap installs the imported native checkpoint; explicit preview promotion increments epoch again. No native write may use an earlier epoch.

## Native Ref mutation failures

The native Ref update, idempotency journal, audit/outbox record, and generation increment share an SQLite IMMEDIATE transaction. If a client loses the response, it may repeat the same idempotency key and exact request. A conflicting reuse is rejected. CAS conflicts do not advance the Ref. Orphan immutable native objects can remain after crash, but they do not imply accepted Ref state.

## Native to Git fallback

The gateway first requires a verified mirror at the current native generation. It moves the authority to `FALLBACK_FROZEN` in SQLite, incrementing epoch to block native writes. It then CAS-updates Git `refs/heads/main` against the original cutover checkpoint. If Git already points to the exact exported OID, it treats the CAS step as previously completed. Any other unexpected Git head is a split-brain error and blocks fallback. Finally the authority is switched to `GIT_AUTHORITY`, incrementing epoch once more.

**Crash after freeze but before Git CAS:** retry fallback with the recorded frozen epoch. **Crash after Git CAS but before SQLite finalize:** retry; the Git target is already correct. **Unexpected external Git write:** stop; quarantine and investigate rather than overwrite.

## Restore

`osc-ops restore-plan` only verifies the provided backup and describes it. It never overwrites Git repositories or service files. Recovery requires a separately tested offline restore procedure with repository-specific object integrity and parity checks. A verified manifest by itself is not proof of disaster recovery readiness.
