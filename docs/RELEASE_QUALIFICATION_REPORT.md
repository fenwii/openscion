# OpenScion V0.6 — Release Qualification Report

**Artifact:** Rust source project. **Intended edition:** Git-backed commercial release candidate with native authority preview. **Result:** NOT QUALIFIED for commercial GA.

## Verified in the package-producing environment

- 22 Cargo crate manifests parse and local path dependencies exist.
- SQLite V3→V4→V5→V6 migration executes, with foreign keys enabled and an `integrity_check` result of `ok`.
- SQL expected-target, expected-generation CAS and ABA rejection scenarios pass in an independent SQLite fixture.
- Previous-version V0.2–V0.5 structural checks pass under the V0.6 superset workspace.
- Git merge, candidate commit/parent, retention, and protected-Ref CAS plumbing tests pass against real temporary Git repositories.
- V0.6 Git mirror update leaves `main` unchanged; stale expected-old Git Ref update is rejected.
- TypeScript SDK compiles with `tsc --noEmit`.
- Python source scripts compile and Bash scripts pass `bash -n`.

## Unverified release gates

- Rust/Cargo compilation and crate-level tests (toolchain unavailable in source-creation environment).
- Full Rust HTTP smoke tests and V0.6 authority cutover/fallback integration test.
- Rustfmt and Clippy results.
- Locked dependency resolution and checked-in `Cargo.lock`.
- Linux and macOS CI runs.
- Power-loss, multiwriter/multihost, worker fencing, filesystem corruption and disaster-recovery drills.
- Independent penetration testing, code audit, signed supply-chain provenance and SBOM.
- 10,000 eligible Git-commit parity corpus and measured operational service objectives.
- Formal G0–G7 commercial release acceptance.

## Risk classification

`GIT_AUTHORITY` is the only default operational mode. `NATIVE_AUTHORITY_PREVIEW` is disabled by default and only accepts previously verified imported Git Changesets. The source files are for testing/review; do not replace production source-control backups or advertise general Git native-authority compatibility.

The project intentionally includes a `release-evidence.json` with all gates marked incomplete; `osc-ops release-verify` fails on this file. To publish a GA release, run the CI suite, fix discovered failures, generate/review the lockfile, provide signed audit and build records, and obtain engineering/security/operations approval.
