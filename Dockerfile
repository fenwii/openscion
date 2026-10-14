# Development build example. For GA, pin image digests and publish signed SBOM/provenance.
FROM rust:1-bookworm AS builder
WORKDIR /source
COPY . .
RUN cargo build --release --workspace
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git && rm -rf /var/lib/apt/lists/*
RUN groupadd --system openscion && useradd --system --gid openscion --home-dir /var/lib/openscion openscion
COPY --from=builder /source/target/release/osc-server /usr/local/bin/osc-server
COPY --from=builder /source/target/release/osc-admin /usr/local/bin/osc-admin
COPY --from=builder /source/target/release/osc-ops /usr/local/bin/osc-ops
COPY --from=builder /source/target/release/osc-shadow /usr/local/bin/osc-shadow
COPY --from=builder /source/target/release/osc-workspace /usr/local/bin/osc-workspace
COPY --from=builder /source/target/release/osc-change /usr/local/bin/osc-change
RUN mkdir -p /var/lib/openscion /srv/openscion/repos && chown openscion:openscion /var/lib/openscion
USER openscion
EXPOSE 8787
ENTRYPOINT ["osc-server"]
CMD ["--data", "/var/lib/openscion", "--bind", "127.0.0.1:8787"]
