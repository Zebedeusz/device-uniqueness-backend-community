# Identity Backend V2

Rust backend that proves a device and registers a username on Polkadot's People Chain.

Three binaries with service-owned Postgres databases:

- **`identity-api`** — HTTP API: auth handshake (challenge → attestation → JWT), `POST /api/v1/usernames`, JWKS, health.
- **`identity-chain-writer`** — single-instance worker that drains the reservation outbox onto the chain.
- **`username-indexer`** — finalized-chain username projection and public prefix search.

Current architecture and scope: [docs/architecture.md](docs/architecture.md) · Delivery status: plans under [docs/plans/](docs/plans/)

## Run locally

```bash
docker compose up          # identity API :8080, username search :8081
```

Or natively: `just run` (api) / `just run-writer`, with env from `.env.example`.

## Develop

```bash
just check                 # fmt + clippy + tests (same as CI)
```

## Deploy

Push to `main`. CI tests, builds the image, and updates the test server
([workflow](.github/workflows/deploy.yml)).

- Live: <https://identity.dotspark.app> · [API reference](https://identity.dotspark.app/docs)
- From-zero setup: [docs/deploy.md](docs/deploy.md) · Operations: [docs/runbook.md](docs/runbook.md)
