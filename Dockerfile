# syntax=docker/dockerfile:1

# Build both deployable binaries (identity-api, identity-chain-writer) from the workspace
# in one release build. TLS is pure-Rust (ring + rustls), so no OpenSSL/system
# TLS libs are needed. Migrations are embedded at compile time (sqlx::migrate!),
# so the build needs the source tree but not a live database.
FROM rust:1.95-bookworm AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release -p identity-service --bin identity-api --bin identity-chain-writer \
    && mkdir -p /out \
    && cp target/release/identity-api target/release/identity-chain-writer /out/

# Minimal glibc runtime (matches the bookworm builder's glibc target).
# curl exists only for the compose healthcheck hitting /readyz — drop it for a
# hardened prod image and use Kubernetes httpGet probes instead.
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --user-group appuser
COPY --from=builder /out/identity-api /out/identity-chain-writer /usr/local/bin/
USER appuser
EXPOSE 8080
# Default to the API; the worker overrides this with `identity-chain-writer`.
CMD ["identity-api"]
