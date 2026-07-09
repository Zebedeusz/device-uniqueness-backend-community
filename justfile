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

# Dependency license/advisory audit (requires `cargo install cargo-deny`).
deny:
    cargo deny check

# Run the identity-api service.
run:
    cargo run -p identity-service --bin identity-api

# Run the identity-chain-writer worker (drains the reservation outbox onto People Chain).
run-writer:
    cargo run -p identity-service --bin identity-chain-writer

# Manual fallback deploy of the local working copy (normal path: CI deploys on push to main).
# Overrides: DEPLOY_HOST (default ibv2), DEPLOY_URL (default https://identity.dotspark.app).
deploy:
    ./scripts/deploy.sh
