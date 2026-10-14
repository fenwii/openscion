# OpenScion V0.6 — Formal Release Gates

**No gate is asserted as passed by this source package.** `release-evidence.json` explicitly records all gates as pending. `osc-ops release-verify` fails unless a documented evidence record proves all of them.

| Gate | Required objective evidence |
|---|---|
| G0 — Baseline | Successful `cargo build/test/clippy/fmt` of V0.1–V0.5 on supported platforms and locked dependency graph |
| G1 — Integrity | Canonical Blob/Tree/Changeset hash fixtures, corruption detection, reverified Git/native mappings |
| G2 — Concurrency | Git CAS no lost updates, native epoch/generation ABA, duplicate retries, 10/100 Agent workloads |
| G3 — Recovery | Crash injection at Git/SQLite/CAS boundaries, outage/fallback tests, independently restored backups |
| G4 — Security | External audit, OS-level Git writer fencing, sandbox controls, least privilege and secret handling |
| G5 — Compatibility | Exact supported Git feature matrix, cross-version API/storage migrations, SHA-1/SHA-256 tests |
| G6 — Operations | Monitoring, measured SLOs, disaster recovery, support runbooks, customer rollout approval |
| G7 — Release | Reviewed `Cargo.lock`, SBOM, licenses, reproducible CI, signed binaries/provenance and approval |

Native authority is not commercial GA in V0.6. Passing local preview qualification or moving imported native references does not satisfy these release gates.

Build and verification checklist:

```sh
cargo generate-lockfile
cargo fmt --all -- --check
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
python3 scripts/check_v06.py
python3 scripts/test_git_contract_v06.py
python3 scripts/v06-smoke.py
cd sdk/typescript && tsc --noEmit
```

`RELEASE_VERIFICATION.json` must accurately distinguish checks actually completed from those not run. A source release package is not a binary artifact and does not imply security or commercial certification.
