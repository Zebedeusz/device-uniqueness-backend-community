# syntax=docker/dockerfile:1

# One build, ten images.
#
# The `builder` stage compiles every deployable binary in a SINGLE
# `cargo build --release` — that is the expensive half, and it is shared by all
# ten service stages below, so `docker buildx bake all` costs one compilation,
# not ten. Each service stage then copies exactly one binary, which is what
# makes the published images small and independently scannable.
#
# There is NO all-in-one stage and no meaningful default target: every consumer
# passes `--target` (`docker-bake.hcl`, the `build.target` keys in
# docker-compose.yml, and both CI lanes). A bare `docker build .` builds the
# last stage in this file (`invite-tickets-pool`) and is almost certainly not
# what you want — use `docker buildx bake <service>`.
#
# TLS is pure-Rust (ring + rustls), so no OpenSSL/system TLS libs are needed.
# Migrations are embedded at compile time (sqlx::migrate!), so the build needs
# the source tree but not a live database.

FROM rust:1.95-bookworm AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release \
        -p identity-service --bin identity-api --bin identity-chain-writer --bin registration-queue \
        -p username-indexer --bin username-indexer \
        -p dim-tickets --bin dim-tickets-api --bin dim-tickets-writer \
        -p invite-tickets --bin invite-tickets-api --bin invite-tickets-pool \
        -p turn --bin turn-api \
        -p notifications --bin notify-relay \
    && mkdir -p /out \
    && cp target/release/identity-api target/release/identity-chain-writer \
          target/release/registration-queue \
          target/release/username-indexer \
          target/release/dim-tickets-api target/release/dim-tickets-writer \
          target/release/invite-tickets-api target/release/invite-tickets-pool \
          target/release/turn-api \
          target/release/notify-relay /out/

# The bare binaries, for the release workflow to export with
# `--target binaries --output type=local`. It reuses the builder layer the images
# are built from, so the released tarballs and the released images hold the same
# bytes. Placed here, right after the builder: it must NOT be last, because the
# last stage is what a bare `docker build .` produces.
FROM scratch AS binaries
COPY --from=builder /out/ /

# --- runtime bases -----------------------------------------------------------

# Minimal glibc runtime (matches the bookworm builder's glibc target).
# ca-certificates is NOT optional: every service dials a wss:// chain RPC, and
# without the trust store that fails at runtime, in this service only — which is
# why the Phase 1 demo checks /readyz on every service rather than one.
FROM debian:bookworm-slim AS runtime-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Explicit GID, not `--user-group`: that picks the next free system GID (999
# here), so a manifest saying `runAsGroup: 10001` would be describing a group the
# image does not have — and an fsGroup-mounted secret would be unreadable.
RUN groupadd --system --gid 10001 appuser \
    && useradd --system --uid 10001 --gid 10001 appuser

# The four headless workers stop at `runtime-base`: no HTTP server, so no
# healthcheck, so no curl. In Kubernetes they are probed on the metrics port by
# the kubelet, which needs nothing inside the image.
#
# The six HTTP services add curl, used ONLY by the compose healthcheck hitting
# /readyz. Kubernetes uses httpGet probes instead, so a hardened cluster-only
# image could drop this layer.
FROM runtime-base AS runtime-http
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

# --- HTTP services -----------------------------------------------------------

FROM runtime-http AS identity-api
COPY --from=builder /out/identity-api /usr/local/bin/
USER appuser
EXPOSE 8080
CMD ["identity-api"]

FROM runtime-http AS username-indexer
COPY --from=builder /out/username-indexer /usr/local/bin/
USER appuser
EXPOSE 8080
CMD ["username-indexer"]

FROM runtime-http AS dim-tickets-api
COPY --from=builder /out/dim-tickets-api /usr/local/bin/
USER appuser
EXPOSE 8080
CMD ["dim-tickets-api"]

FROM runtime-http AS invite-tickets-api
COPY --from=builder /out/invite-tickets-api /usr/local/bin/
USER appuser
EXPOSE 8080
CMD ["invite-tickets-api"]

FROM runtime-http AS turn-api
COPY --from=builder /out/turn-api /usr/local/bin/
USER appuser
EXPOSE 8080
CMD ["turn-api"]

FROM runtime-http AS notify-relay
COPY --from=builder /out/notify-relay /usr/local/bin/
USER appuser
EXPOSE 8080
CMD ["notify-relay"]

# --- headless workers --------------------------------------------------------
#
# No EXPOSE: the only listener is the Prometheus exporter on METRICS_ADDR
# (:9090), reachable on the private metrics network, never published.

FROM runtime-base AS identity-chain-writer
COPY --from=builder /out/identity-chain-writer /usr/local/bin/
USER appuser
CMD ["identity-chain-writer"]

FROM runtime-base AS registration-queue
COPY --from=builder /out/registration-queue /usr/local/bin/
USER appuser
CMD ["registration-queue"]

FROM runtime-base AS dim-tickets-writer
COPY --from=builder /out/dim-tickets-writer /usr/local/bin/
USER appuser
CMD ["dim-tickets-writer"]

FROM runtime-base AS invite-tickets-pool
COPY --from=builder /out/invite-tickets-pool /usr/local/bin/
USER appuser
CMD ["invite-tickets-pool"]
