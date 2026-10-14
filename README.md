# OpenScion V0.6 — Commercial Release Candidate (Rust)

OpenScion V0.6 is an **English-language, 22-crate Rust source project** built directly on the V0.5 source tree. It integrates Git-backed source queries, Agent worktrees, controlled Git landing, native Changeset shadow verification, authority fencing, native Ref CAS preview, Git compatibility mirrors, offline backup verification, and a formal release-evidence gate.

> **IMPORTANT RELEASE STATUS: UNQUALIFIED SOURCE CANDIDATE.** The V0.5 source was not compiler-verified in its delivery environment, and Rust/Cargo are unavailable here. V0.6 is **not** certified commercial GA, does **not** include signed release binaries, and must not become the only copy of valuable customer source. No benchmark/SLO or release-gate passage is claimed without evidence. See `docs/v0.6/RELEASE_GATES.md` and `RELEASE_VERIFICATION.json`.

## Product modes

| Mode | Status in source | Write authority |
|---|---|---|
| `GIT_AUTHORITY` | Default implementation; requires CI/security/recovery qualification | Existing Git refs via V0.4 controlled land |
| Shadow V0.5 | Observational and non-authoritative | Git only |
| `CUTOVER_FROZEN` | Operator-controlled stop-the-world preview checkpoint | No application mutation |
| `NATIVE_AUTHORITY_PREVIEW` | **Opt-in experimental**; only existing verified Git-imported Changesets can advance a native Ref | Native SQLite Ref CAS with epoch + generation |
| `FALLBACK_FROZEN` | Recoverable interim state | No mutation |
| `QUARANTINED` | Incident isolation | No mutation |

The V0.5 `OSCCSET\0` format contains immutable Git provenance. It does not yet support independently authored native-only commits. **The V0.6 implementation intentionally does not fabricate source origins or offer arbitrary native commit creation.** Git mirrors use verified original Git commit mappings and are updated only under `refs/openscion/mirror/`.

## Requirements

- Rust stable, Cargo, Git 2.39+ recommended.
- SQLite through bundled `rusqlite`.
- Python 3.11+ for offline tests.
- Node.js / TypeScript for SDK checks.
- Linux and macOS target (cross-platform checks required before GA).
- A single trusted host; no multi-host authority consensus.

## Build and test

```sh
cargo generate-lockfile            # required until a CI-generated lockfile is reviewed and committed
cargo fmt --all
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
python3 scripts/check_v02.py
python3 scripts/check_v03.py
python3 scripts/check_v04.py
python3 scripts/check_v05.py
python3 scripts/check_v06.py
python3 scripts/test_git_contract_v04.py
python3 scripts/test_git_contract_v05.py
python3 scripts/test_git_contract_v06.py
python3 scripts/v06-smoke.py        # requires built osc-server + osc-admin binaries
cd sdk/typescript && tsc --noEmit
```

**Passing structural checks does not mean Rust compilation or release qualification passed.** Run all checks on both target platforms.

## Local setup: Git-backed mode

```sh
cargo build --workspace
./target/debug/osc-admin init --data ./var/openscion --allow-root ./repositories
./target/debug/osc-admin register --data ./var/openscion --name demo --path ./repositories/demo
./target/debug/osc-server --data ./var/openscion --bind 127.0.0.1:8787
./target/debug/osc-ops --data ./var/openscion doctor
```

`osc-admin init` prints an administrative token **once**. Store it securely. Service defaults to loopback. Do not expose bearer-token APIs without TLS and network access control.

## Readiness checks and native preview sequence

Native cutover requires an independently implemented OS-level Git writer fence and verified backup. It is **not** a supported commercial GA operation. Use these only in an isolated test environment:

1. Register Git repository; enable/import its V0.5 shadow history and verify parity.
2. Stop all writers and shadow workers for backup; run `osc-ops backup --all-writers-stopped true`.
3. Run `osc-ops qualify --repo <repo-id>` and `osc-ops preflight --repo <repo-id>`.
4. After external Git write fencing, run `osc-ops freeze` with exact Git/native checkpoint, authority epoch and a verified backup ID.
5. Run `osc-ops bootstrap` for `refs/heads/main` in frozen mode.
6. Enable preview explicitly with `osc-ops feature-preview --enable true`, then `osc-ops promote --external-writers-fenced true`.
7. Native Ref CAS requires expected target, generation, epoch, idempotency key, and policy-evidence identifier.
8. Run `osc-ops mirror` and verify mirror status. Native-to-Git fallback requires no unexported Ref generation and external write fencing.
9. Run `osc-ops fallback` to restore Git authority; abort and quarantine if any verification fails.

Use `osc-ops --help` and its subcommand help for authoritative argument syntax. A passing `qualify` operation demonstrates **local preview eligibility**, not 10,000-commit production parity, signing, or formal commercial approval.

## Key dependencies

- **Native CAS & Changesets:** V0.1 + V0.5, formats unchanged.
- **Git queries / workspaces:** V0.2 + V0.3, Git subprocess backend.
- **Git landing:** V0.4, expected-old-OID CAS.
- **Shadow import:** V0.5, verified Git-native mappings.
- **Authority:** V0.6 `openscion-authority`, authority epochs and repository freeze.
- **Native Ref:** V0.6 `openscion-native-ref`, transactional CAS and closure checks.
- **Git mirror:** V0.6 `openscion-gateway`, verified imported mapping only, recoverable fallback.
- **Operations:** V0.6 `openscion-operations`, offline backup and integrity manifest.
- **Release:** V0.6 `openscion-release`, G0–G7 evidence validation.

## Known limitations

1. No Rust compilation/runtime verification in the producing environment; no `Cargo.lock` has been generated.
2. No independent native-only Changeset authoring, general Git exporter, push/fetch compatibility, or multi-region consensus.
3. Operators must fence external Git writers at the OS level; application permissions cannot stop a direct filesystem writer.
4. The Git mirror supports only previously imported/reverified Git commits.
5. External application-validation evidence remains a trust assumption from V0.4, not cryptographically attested sandbox execution.
6. Offline backup requires quiescence; the `--all-writers-stopped` flag is an operator assertion, not a physical guarantee.
7. Restore is intentionally a verified **non-destructive plan**, not an untested overwriting operation.
8. Full disaster-recovery exercises, durable multiworker tests, production penetration tests, signed provenance, support SLAs and rollout approval remain unverified.

## Documentation

- [Full V0.6 commercial architecture](docs/V06_COMMERCIAL_ARCHITECTURE.md)
- [Implementation architecture](docs/v0.6/ARCHITECTURE.md)
- [HTTP/CLI API](docs/v0.6/API.md)
- [Security](docs/v0.6/SECURITY.md)
- [Authority/fallback recovery](docs/v0.6/RECOVERY.md)
- [Backup and operations](docs/v0.6/OPERATIONS.md)
- [Commercial G0–G7 release gates](docs/v0.6/RELEASE_GATES.md)

MIT license carried forward; third-party and branding review required before commercial distribution.
