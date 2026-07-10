# Identity Backend V2

Rust backend that proves a device and registers a username on Polkadot's People Chain.

Two binaries, one Postgres:

- **`identity-api`** — HTTP API: auth handshake (challenge → attestation → JWT), `POST /api/v1/usernames`, JWKS, health.
- **`identity-chain-writer`** — single-instance worker that drains the reservation outbox onto the chain.

Design and scope: [docs/design-doc.md](docs/design-doc.md)

## Run locally

```bash
docker compose up          # Postgres + api + writer on :8080
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
