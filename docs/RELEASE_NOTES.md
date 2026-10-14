# OpenScion 0.6.0 — Source Candidate Release Notes

**Status:** source implementation delivered; compiler and operational qualification pending. No signed commercial GA tag should be issued.

Changes relative to 0.5:

- Five new Rust crates: authority, native refs, Git compatibility gateway, operations and release-evidence validation.
- Additive SQLite V6 schema; Git remains the default authority.
- Operator-controlled authority freeze, native preview bootstrap/promotion, quarantine and recoverable fallback.
- Native Ref CAS requires native-object closure validation, monotonic epoch, expected target, expected generation, idempotency and an audited event.
- No native-only Changeset authoring; verified imported history only, preserving V0.5 object format.
- One-way, parity-checked Git mirror under `refs/openscion/mirror/*`.
- Offline `VACUUM INTO` SQLite snapshot, native data copy, reachable Git bundle, SHA-256 manifest, integrity verification and non-destructive restore plan.
- Read-only authority/mirror API plus feature-gated native Ref CAS API; privileged cutover/fallback stays local-CLI-only.
- Additional V0.6 real-Git integration test, HTTP smoke and offline schema/Git contracts.
- Deployment templates, release-evidence G0–G7 framework and formal architecture documentation.

Known gaps: no Rust toolchain available during construction; no compiled binaries or Cargo.lock, no CI pass, no multi-host fencing, no independently signed validation attestation, no live disaster-recovery qualification, no native-only authoring/export, no commercial GA approval.
