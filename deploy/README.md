# Deployment notes (source candidate)

The `Dockerfile` and systemd unit are operational templates **not security-certified deployables**. They do not bundle TLS: bind on loopback and place a properly configured TLS gateway and authorization controls ahead of any remote service. With container networking, a loopback-bound process cannot be reached across a published port; explicitly design proxy sidecar/shared-network or validated bind topology instead of blindly exposing a public listener.

The service process must have write access to the configured Git repository only when running Git-authoritative landing. Native authority preview requires a separately enforced OS-level fence that prevents unauthorized Git writes. Do not run two V0.6 instances with independent authority metadata against the same repository. Do not mount untrusted Agent-generated code with access to Git administrative directories.

Commercial GA additionally requires pinned base image digests, package/SBOM scanning, health/metrics, signed supply-chain provenance, runbook exercises and a reviewed dependency lockfile.
