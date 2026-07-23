# syntax=docker/dockerfile:1

# Build all deployable binaries (identity-api, identity-chain-writer,
# username-indexer, dim-tickets-api, dim-tickets-writer, invite-tickets-api,
# invite-tickets-pool, turn-api, notify-relay) from the workspace in one release
# build. TLS is pure-Rust (ring + rustls), so no OpenSSL/system TLS libs are
# needed. Migrations are embedded at compile time (sqlx::migrate!), so the build
# needs the source tree but not a live database.
FROM rust:1.95-bookworm AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release \
        -p identity-service --bin identity-api --bin identity-chain-writer \
        -p username-indexer --bin username-indexer \
        -p dim-tickets --bin dim-tickets-api --bin dim-tickets-writer \
        -p invite-tickets --bin invite-tickets-api --bin invite-tickets-pool \
        -p turn --bin turn-api \
        -p notifications --bin notify-relay \
    && mkdir -p /out \
    && cp target/release/identity-api target/release/identity-chain-writer \
          target/release/username-indexer \
          target/release/dim-tickets-api target/release/dim-tickets-writer \
          target/release/invite-tickets-api target/release/invite-tickets-pool \
          target/release/turn-api \
          target/release/notify-relay /out/

# Minimal glibc runtime (matches the bookworm builder's glibc target).
# curl exists only for the compose healthcheck hitting /readyz — drop it for a
# hardened prod image and use Kubernetes httpGet probes instead.
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --user-group appuser
COPY --from=builder /out/identity-api /out/identity-chain-writer /out/username-indexer /out/dim-tickets-api /out/dim-tickets-writer /out/invite-tickets-api /out/invite-tickets-pool /out/turn-api /out/notify-relay /usr/local/bin/
USER appuser
EXPOSE 8080
# Default to the API; the worker, username-indexer, and notify-relay services
# override this with their own command.
CMD ["identity-api"]
