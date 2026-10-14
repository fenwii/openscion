# Contributing to OpenScion V0.1

The persistent object format and crash recovery rules are compatibility boundaries. Include a design note when changing object serialization, the hashing preimage, snapshot database publication, or file-system safety behavior.

## Before a change

1. Read `docs/OBJECT_FORMAT.md` and `docs/ARCHITECTURE.md`.
2. Identify affected invariants and filesystem operations.
3. Add deterministic unit tests and a regression test.
4. Run `cargo fmt --all`, `cargo test --workspace`, `cargo clippy --workspace --all-targets`, and `bash scripts/smoke.sh`.
5. For changes to persistence paths, add a crash/restart test.

## Version scope

Do not add Git commits, branches, Agent runtime, network API, or native parent-linked Changesets to V0.1. They are planned for V0.2–V0.6.

## Reporting bugs

Include OS, filesystem, Rust version, repository size, exact command, error text, and a safe reproduction that excludes confidential source data.

## V0.4 safety requirements

Do not change Git ref mutation semantics, candidate retention, SQLite cross-store recovery, or policy encoding without dedicated review. Run `cargo test --workspace`, V0.4 Git fixture tests, and all earlier regression suites. The V0.4 built-in validation check proves only Git object integrity, not application correctness. External runner check claims must remain bound to the exact candidate and policy digest.

## V0.6 native shadow contribution requirements

The V0.6 `OSCCSET\0` binary format, Changeset domain-separated hashing rule, first-parent delta semantics, ordered parent graph, and existing V0.1 CAS/Tree formats are protocol-critical. Any format change requires a separate encoding version, golden fixtures, a migration plan and maintainer review.

Changes to shadow import must preserve Git as the only authoritative history. The new subsystem must not modify protected refs, worktree HEADs, or V0.4 landing state. Native-derived indexes must remain reconstructible from immutable inputs and verified against exact Git tracked paths, modes and file bytes. Reject unsupported Gitlinks, dangerous paths and missing objects explicitly.

Run `cargo fmt --all -- --check`, `cargo build --workspace`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`, and all `scripts/check_v0*.py` checks. Run real-Git and HTTP smoke tests on Linux and macOS. The project in this source package has **not** been compiler-verified in its creation environment.

## V0.6 authority changes

All modifications to authority transitions, native CAS, Git mirror, and offline backup require a documented ADR, a negative-path test and explicit human code review. Do not change the canonical `OSCCSET` identity format without a versioned migration. Never claim GA until G0–G7 are evidenced.
