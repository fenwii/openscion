# OpenScion V0.6 Security Notice

OpenScion V0.6 is an **unverified developer-preview source implementation**, not a production-qualified source-control server. Git remains authoritative. V0.6 native mapping and indexes are shadow-only and cannot update Git refs.

The V0.4 landing subsystem and V0.3 Worktree subsystem remain included; their existing security limitations also apply. A Git Worktree is **not** an operating-system execution sandbox, and a built-in Git tree integrity check is **not** an application test suite.

Register and import trusted Git repositories only. Authentication and source authorization must be enforced before exposing private code or shadow mappings. No arbitrary repository hooks, filters, external diff drivers or untrusted code should be run with service credentials. Operator tokens must be stored securely, rotated, and not printed to logs.

The V0.6 worker supports lease generation fencing and heartbeat renewal, but full multi-worker fault-injection and denial-of-service testing remain mandatory before exposure to untrusted clients. Import has explicit Blob, path, file count and commit count limits.

Consult [V0.6 Security](docs/v0.5/SECURITY.md), [V0.6 Recovery](docs/v0.5/RECOVERY.md), [V0.6 Verification](docs/v0.5/VERIFICATION.md), and [V0.4 Security](docs/v0.4/SECURITY.md).

Please report vulnerabilities privately to the maintainers; do not share credentials or repository contents in issue reports.


## V0.6 commercial authority boundary

The native authority implementation is feature-gated, disabled by default, and accepts only verified imported Changesets. Repository cutover requires external writer fencing, an offline verified backup, parity qualification and operator approval. A flag is not a substitute for OS-level locking or access control. See [V0.6 Security Model](docs/v0.6/SECURITY.md). The current source archive is not production-certified.
