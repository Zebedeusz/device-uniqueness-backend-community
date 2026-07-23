default: check

# Full local gate — matches CI.
check: fmt-check lint test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# On-demand live-Postgres tests (NOT in `just check`; needs Docker). A live-Postgres
# CI dependency is deliberately deferred, so the `#[ignore]`d live tests run here:
# this spins up the username-indexer Postgres, runs them, and tears it down.
test-live:
    #!/usr/bin/env bash
    set -euo pipefail
    docker compose up -d username-indexer-postgres
    trap 'docker compose rm -fsv username-indexer-postgres >/dev/null 2>&1 || true' EXIT
    for _ in $(seq 1 30); do
        docker compose exec -T username-indexer-postgres pg_isready -U username_indexer -d username_indexer >/dev/null 2>&1 && break
        sleep 1
    done
    DATABASE_URL=postgres://username_indexer:username_indexer@localhost:5433/username_indexer \
        cargo test -p username-indexer --test pagination_live -- --ignored

# Dependency license/advisory audit (requires `cargo install cargo-deny`).
deny:
    cargo deny check

# Regenerate the committed API reference (docs/api-reference/openapi.json + index.html)
# from the `#[utoipa::path]` annotations. The `committed_artifacts_are_in_sync` test
# in `just check` fails if these are stale, so run this after changing any route/DTO.
openapi:
    cargo run -p apidoc-gen

# Run the identity-api service.
run:
    cargo run -p identity-service --bin identity-api

# Run the identity-chain-writer worker (drains the reservation outbox onto People Chain).
run-writer:
    cargo run -p identity-service --bin identity-chain-writer

# Run the notifications relay (stateless /api/v1/notify).
run-notify:
    cargo run -p notifications --bin notify-relay
