# Device Uniqueness Backend

Rust backend that proves a device and registers a username on Polkadot's People Chain, plus the sibling services the app depends on. Each service owns its Postgres database (where it has one) and deploys independently behind a single-URL [gateway](gateway/Caddyfile).

- **`device-attestation-api`** — HTTP API: auth handshake (challenge → attestation → JWT + refresh), username registration with free/paid lanes (`POST /api/v1/usernames`, availability, payment status, queue status), attester key, JWKS, health.
- **`device-attestation-chain-writer`** — single-instance worker that drains the reservation outbox onto the chain.
- **`registration-queue`** — single-instance queue advancer; while `QUEUE_ENABLED` is on, it is the only path out of the registration queue.
- **`username-indexer`** — finalized-chain username projection and public prefix search, plus the optional proof-of-compute gate.
- **`invite-tickets-api`** / **`invite-tickets-pool`** — synchronous invitation-ticket claim and its keypair-pool maintainer.
- **`turn-api`** — stateless TURN (coturn REST API) credential issuer.
- **`notify-relay`** — stateless APNs / FCM push relay.
- **`voucher-mint`** — operator CLI for minting registration vouchers (deliberately not an HTTP surface).

Current architecture and scope: [docs/architecture.md](docs/architecture.md) · Delivery status: plans under [docs/plans/](docs/plans/)

## Run locally

```bash
docker network create dub-edge   # once; shared with the edge project
docker compose up                 # nothing is published — the edge does that

# to reach a service directly, layer the loopback-only debug overlay:
docker compose -f docker-compose.yml -f docker-compose.debug.yml up -d
#   device-attestation 127.0.0.1:8080 · indexer :8081
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

**One image, eight roles.** The released image is mirrored to public Docker Hub — no
account, no token:

```bash
docker pull paritytech/device-uniqueness-backend:stable
docker run paritytech/device-uniqueness-backend:stable --role device-attestation-api
```

Every service and worker is a `--role` of the single `dub` binary the image holds
(`dub --list-roles` prints the eight); what makes a container a given service is its
role, not a different image. The same digests are on
`ghcr.io/paritytech/device-uniqueness-backend` — `vX.Y.Z` and `stable` exist on both;
**`latest` is ghcr-only**, because it tracks `main` and unreleased work is not what an
outside consumer should pick up by default. The ghcr package inherits this repo's
private visibility, so pulling that needs a token.

Images are multi-arch (amd64 + arm64). Three kinds of tag, and the difference matters:

| Tag | Points at | Use it for |
| --- | --- | --- |
| `vX.Y.Z` | that release, forever | anything reproducible — pin this |
| `stable` | the newest release | "track releases, not `main`" |
| `latest` | tip of `main` | our own test server; **contains unreleased work** |

Each release also attaches `dub-<version>-<target>.tar.gz` — the single `dub` binary
for x86_64/aarch64 Linux and Apple Silicon — and a `dub-compose-*.tar.gz` bundle with
`docker-compose.yml`, `.env.example` and the gateway config. That compose file needs no
source tree: the `build:` stanzas are gone and the image is pinned to that release, so
`docker compose up -d` starts the stack as-is. The build graph is one target — see
[`docker-bake.hcl`](docker-bake.hcl).

Changes per release: [CHANGELOG.md](CHANGELOG.md). Pre-1.0, breaking changes bump
the **minor** — `0.1.x` → `0.2.0` can break you.

To cut a release: bump `version` under `[workspace.package]`, add the matching
`## [X.Y.Z]` changelog section, and merge that as a PR titled `chore(release): vX.Y.Z`
— merging tags the commit and publishes
([workflow](.github/workflows/release-on-merge.yml)). Pushing a `vX.Y.Z` tag by hand
runs the same release. CI refuses a tag that disagrees with the manifest or the changelog
([workflow](.github/workflows/release.yml)).
