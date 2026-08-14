# Identity Backend V2

Rust backend that proves a device and registers a username on Polkadot's People Chain, plus the sibling services the app depends on. Each service owns its Postgres database (where it has one) and deploys independently behind a single-URL [gateway](gateway/Caddyfile).

- **`identity-api`** — HTTP API: auth handshake (challenge → attestation → JWT + refresh), username registration with free/paid lanes (`POST /api/v1/usernames`, availability, payment status, queue status), attester key, JWKS, health.
- **`identity-chain-writer`** — single-instance worker that drains the reservation outbox onto the chain.
- **`registration-queue`** — single-instance queue advancer; while `QUEUE_ENABLED` is on, it is the only path out of the registration queue.
- **`username-indexer`** — finalized-chain username projection and public prefix search, plus the optional proof-of-compute gate.
- **`dim-tickets-api`** / **`dim-tickets-writer`** — DIM (Game / ProofOfInk) invitation-credential service.
- **`invite-tickets-api`** / **`invite-tickets-pool`** — synchronous invitation-ticket claim and its keypair-pool maintainer.
- **`turn-api`** — stateless TURN (coturn REST API) credential issuer.
- **`notify-relay`** — stateless APNs / FCM push relay.
- **`voucher-mint`** — operator CLI for minting registration vouchers (deliberately not an HTTP surface).

Current architecture and scope: [docs/architecture.md](docs/architecture.md) · Delivery status: plans under [docs/plans/](docs/plans/)

## Run locally

```bash
docker network create ibv2-edge   # once; shared with the edge project
docker compose up                 # nothing is published — the edge does that

# to reach a service directly, layer the loopback-only debug overlay:
docker compose -f docker-compose.yml -f docker-compose.debug.yml up -d
#   identity 127.0.0.1:8080 · indexer :8081 · dim-tickets :8082
#   invite-tickets :8083 · turn :8084 · notify :8085
```

Or natively: `just run` (api) / `just run-writer` / `just run-notify`, with env from `.env.example`.

## Develop

```bash
just check                 # fast offline gate: fmt + clippy + workspace tests
just test-live-db          # deterministic Postgres suites; also required by CI
just test-live-chain       # optional Postgres + live People Chain smokes
just test-live-providers   # optional credentialed APNs / FCM smokes
just coverage-db           # canonical offline + Postgres coverage report
just openapi               # regenerate docs/api-reference after route/DTO changes;
                           # a test in `just check` fails if it is stale
```

## Deploy

Push to `main`. CI tests, builds the image, and updates the test server
([workflow](.github/workflows/deploy.yml)).

- Live: <https://identity.dotspark.app> · [API reference](https://identity.dotspark.app/docs) · PreviewNet: <https://identity-previewnet.dotspark.app>
- From-zero setup: [docs/deploy.md](docs/deploy.md) · Operations: [docs/runbook.md](docs/runbook.md)

## Releases

Released images are mirrored to public Docker Hub — no account, no token:

```bash
docker pull paritytech/ibv2-identity-api:stable
```

Docker Hub namespaces are one level deep, so the ten services are
`paritytech/ibv2-<service>` there, and `ghcr.io/paritytech/identity-backend-rust/<service>`
on ghcr. `vX.Y.Z` and `stable` exist on both; **`latest` is ghcr-only**, because it tracks
`main` and unreleased work is not what an outside consumer should pick up by default. The
ghcr packages inherit this repo's private visibility, so pulling those needs a token.

Each release also attaches `ibv2-<version>-<target>.tar.gz` — every service binary for
x86_64/aarch64 Linux and Apple Silicon — and an `ibv2-compose-*.tar.gz` bundle whose
compose file needs no source tree: the `build:` stanzas are gone and all ten images are
pinned to that release, so `docker compose up -d` starts the stack as-is.

Images are published as `ghcr.io/paritytech/identity-backend-rust/<service>` — one per
service, all sharing a tag — multi-arch
(amd64 + arm64). Three kinds of tag, and the difference matters:

| Tag | Points at | Use it for |
| --- | --- | --- |
| `vX.Y.Z` | that release, forever | anything reproducible — pin this |
| `stable` | the newest release | "track releases, not `main`" |
| `latest` | tip of `main` | our own test server; **contains unreleased work** |

Every service has its own image holding exactly its own binary (one `cargo build`, ten
thin runtime stages — see [`docker-bake.hcl`](docker-bake.hcl)), wired together by
`docker-compose.yml`;
each release attaches an `ibv2-compose-*.tar.gz` with that file, `.env.example`
and the gateway config, so a standalone stack has something to start from.

Changes per release: [CHANGELOG.md](CHANGELOG.md). Pre-1.0, breaking changes bump
the **minor** — `0.1.x` → `0.2.0` can break you.

To cut a release: bump `version` under `[workspace.package]`, add the matching
`## [X.Y.Z]` changelog section, and merge that as a PR titled `chore(release): vX.Y.Z`
— merging tags the commit and publishes
([workflow](.github/workflows/release-on-merge.yml)). Pushing a `vX.Y.Z` tag by hand
runs the same release. CI refuses a tag that disagrees with the manifest or the changelog
([workflow](.github/workflows/release.yml)).
