# V0.6 Commercial Operations Runbook (Release Candidate)

## Development / preproduction

- Install Rust/Cargo and supported Git on Linux and macOS.
- Generate, review and commit `Cargo.lock` before producing a release.
- Run `cargo build --workspace`, `cargo test --workspace`, Clippy, rustfmt and every Python/HTTP smoke test.
- Validate SQLite migration from a backed-up copy of an actual V0.5 service database.
- Start the server on `127.0.0.1` by default. Only expose through a validated TLS edge and ingress policies.
- Restrict Git object-store write access to authorized landing and mirror service users.

## Backups

During a maintenance window, stop the service, shadow workers, Git receives and all other writers. Use `osc-ops backup` with `--all-writers-stopped true`. The tool uses `VACUUM INTO` for SQLite and copies native CAS bytes and configuration, creates Git `bundle --all` snapshots, and produces a SHA-256 manifest. Verify the archive after creation; independently test the restore of each Git bundle into a clean repository. The backup excludes unreachable Git objects not retained by refs, so correct pinning and retention remain essential.

## Monitoring / diagnostics

Track Ref CAS conflicts, authority epochs, quarantine state, mirror lag, shadow verification failures, worker lease/fencing errors, backup age, Git object integrity errors, SQLite database integrity and outbox backlog. The current package provides CLI-readable state and historical diagnostics; it does not guarantee a production-grade alerting or tracing deployment.

## Upgrades

Versioned SQLite V6 migration is additive. A V0.5 operator should create a verified backup before invoking V0.6 `Registry::open` for the first time, because it automatically applies newer schema migrations. Unknown newer database formats must not be written. Do not perform rolling upgrades across independent writers.

## Commercial response

Commercial GA requires measured SLOs, RPO/RTO disaster-recovery exercise, security assessment, customer support escalation ownership, signed release artifacts and repeatable clean-room deployment. Until completed, do not offer contractual uptime/durability guarantees.
