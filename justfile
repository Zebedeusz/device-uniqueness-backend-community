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
# CI dependency is deliberately deferred, so the `#[ignore]`d live tests run here.
test-live:
    #!/usr/bin/env bash
    set -euo pipefail
    identity_test_port="${IDENTITY_TEST_POSTGRES_PORT:-55432}"
    export IDENTITY_POSTGRES_PORT="$identity_test_port"
    docker compose up -d postgres username-indexer-postgres
    trap 'docker compose rm -fsv postgres username-indexer-postgres >/dev/null 2>&1 || true' EXIT
    for _ in $(seq 1 30); do
        docker compose exec -T postgres pg_isready -U identity -d identity >/dev/null 2>&1 && break
        sleep 1
    done
    IDENTITY_TEST_DATABASE_URL="postgres://identity:identity@localhost:${identity_test_port}/identity" \
        cargo test -p identity-service --test allocation_live -- --ignored
    IDENTITY_TEST_DATABASE_URL="postgres://identity:identity@localhost:${identity_test_port}/identity" \
        cargo test -p identity-service --test queue_live -- --ignored
    IDENTITY_TEST_DATABASE_URL="postgres://identity:identity@localhost:${identity_test_port}/identity" \
        cargo test -p identity-service --test voucher_live -- --ignored
    IDENTITY_TEST_DATABASE_URL="postgres://identity:identity@localhost:${identity_test_port}/identity" \
        cargo test -p identity-service --test payment_live -- --ignored
    # End-to-end through the production router; these also need a reachable
    # People Chain RPC (PEOPLE_RPC_URL or the Paseo default).
    IDENTITY_TEST_DATABASE_URL="postgres://identity:identity@localhost:${identity_test_port}/identity" \
        cargo test -p identity-service --test voucher_http_live -- --ignored
    IDENTITY_TEST_DATABASE_URL="postgres://identity:identity@localhost:${identity_test_port}/identity" \
        cargo test -p identity-service --test payment_http_live -- --ignored
    IDENTITY_TEST_DATABASE_URL="postgres://identity:identity@localhost:${identity_test_port}/identity" \
        cargo test -p identity-service --test payment_watch_live -- --ignored
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
