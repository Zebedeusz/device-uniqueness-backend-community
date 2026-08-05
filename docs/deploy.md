# Deploy

From-zero setup of a server. Day-2 operations: [runbook.md](runbook.md).

Topology: `identity-api`, `identity-chain-writer`, and the single-instance `registration-queue` advancer (no signing secret; with `QUEUE_ENABLED` on it is the queue's only drain — down, claims park as `QUEUED` and the writer warns about stranded rows) share the identity Postgres; `username-indexer` owns a separate projection Postgres; `dim-tickets-api` and `dim-tickets-writer` share a third, dim-tickets-owned Postgres; `invite-tickets-api` and the single-instance `invite-tickets-pool` share a fourth, invite-tickets-owned Postgres (the ticket pool holds private keys — nothing shared). `turn-api` and `notify-relay` are stateless (no Postgres). Normal updates are done by CI ([workflow](../.github/workflows/deploy.yml)) — this page is only for standing up a new machine.

**Two compose projects, one host.** The services live in an *environment* project started from [`docker-compose.yml`](../docker-compose.yml); the public edge is a separate project started from [`gateway/docker-compose.yml`](../gateway/docker-compose.yml). They meet on a shared external docker network (`ibv2-edge`) that neither owns, where each environment registers its services under `ENV_ID`-suffixed aliases (`identity-api-paseo-next-v2`, …). The edge resolves those aliases by docker DNS and is **the only container that publishes a host port** (80/443): nothing else is reachable from outside the machine, and a second environment collides with nothing. Route table: the committed [`gateway/Caddyfile`](../gateway/Caddyfile), loaded by the edge container. Ports for local debugging come from the opt-in [`docker-compose.debug.yml`](../docker-compose.debug.yml) overlay, which binds to `127.0.0.1` only.

Secret boundaries: the JWT **signing** seed lives only in `identity-api`; `dim-tickets-api`, `invite-tickets-api`, `turn-api`, and `notify-relay` get the **public** key (or JWKS) only. Each chain-submitting worker holds only its own signing secret: the dim inviter SURI lives only in `dim-tickets-writer`, the invite inviter SURI only in `invite-tickets-pool`. `turn-api` additionally holds `TURN_SECRET` — the HMAC key shared with the TURN relay (coturn `--use-auth-secret`); relay and issuer must be rotated together, and no other service sees it. Push credentials (`APNS_*`/`FCM_*`) live only in `notify-relay`. Compose uses the root `.env` for interpolation only and gives every application container an explicit environment allowlist, so no service can read another's key.

**Nonce lanes**: `dim-tickets-writer` and `invite-tickets-pool` each submit as "an inviter" and MUST NOT sign as the same account — two independent submitters on one account race nonces. Give each its own account (e.g. separate proxy delegates of one cold inviter primary); compose interpolates them from distinct `INVITER_*` (dim) and `INVITE_INVITER_*` (invite) host variables.

Postgres is reachable by sibling containers on the internal Compose network and publishes no host port at all; set a URL-safe random `DIM_TICKETS_POSTGRES_PASSWORD` on deployments.

## Prerequisites

- Ubuntu server with sudo and outbound internet (services connect to a People Chain RPC at startup)
- DNS A record pointing the domain at the server IP (direct, not proxied)
- Ports 80/443 open; the attester signing key (ask the IB team)

## 1. Install Docker and create the shared network

Caddy is not installed on the host — it runs as the edge container.

```bash
sudo apt-get update
sudo apt-get install -y docker.io docker-compose-v2 ca-certificates curl gnupg
sudo systemctl enable --now docker

# Owned by no project: both the environment and the edge attach to it.
sudo docker network create ibv2-edge

# Same idea for metrics: the environment and the observability project meet
# here, and nothing else does (Prometheus is never reachable from the edge).
sudo docker network create ibv2-metrics
```

## 2. Code and config

```bash
git clone git@github.com:paritytech/identity-backend-rust.git ~/identity-backend-v2
cd ~/identity-backend-v2
```

`ENV_ID` names this environment and is what the network aliases are suffixed with. It defaults to `paseo-next-v2` (the ecosystem's id for this network); a second environment sets its own, e.g. `previewnet`. It is unrelated to `PEOPLE_NETWORK`, which stays the frozen legacy literal `paseo`.

Create `.env` (`chmod 600 .env`):

```bash
BIND_ADDR=0.0.0.0:8080
PEOPLE_RPC_URL=wss://paseo-people-next-system-rpc.polkadot.io
JWT_ED25519_SECRET=0x<64-hex>            # openssl rand -hex 32
ATTESTER_PUBLIC_KEY=0x<64-hex>           # served by GET /api/v1/attester
AUTH_ENABLED=false                       # test mode: attestation is a no-op
ENFORCE_AUTH=false
RUST_LOG=info
CHAIN_WRITER_SIGNER_SURI=<secret>        # mnemonic, 0x+64-hex seed, or 0x+128-hex raw sr25519
CHAIN_WRITER_PROXY_FOR=<attester SS58 or empty>
# dim-tickets (see .env.example for tunables and dev defaults)
DIM_TICKETS_POSTGRES_PASSWORD=<URL-safe random password>
PEOPLE_NETWORK=<westend2|paseo|polkadot>
INVITER_ADDRESS=<dim inviter SS58>        # its on-chain AvailableInvites funds dim tickets
JWT_ED25519_PUBLIC_KEY=0x<64-hex>         # PUBLIC half of JWT_ED25519_SECRET (or set JWT_JWKS_JSON)
INVITER_SIGNER_SURI=<secret>              # same accepted formats as the chain writer
INVITER_PROXY_FOR=<inviter primary SS58 or empty>
# invite-tickets (distinct account from the dim inviter — see Nonce lanes above)
INVITE_INVITER_ADDRESS=<invite inviter SS58>   # funds set_invite_ticket; stamped into ticket rows
INVITE_INVITER_SIGNER_SURI=<secret>            # hot invite inviter key (or its proxy delegate)
INVITE_INVITER_PROXY_FOR=<inviter primary SS58 or empty>

# turn-api (stateless credential issuer; the secret is shared with the coturn relay)
TURN_SECRET=<base64 of the relay's coturn static-auth-secret>   # base64 the RAW secret string; do not hex-decode it first
TURN_AUTH_ALGORITHM=SHA1                  # must match the relay
TURN_TTL_SECS=1800
TURN_REALM=<relay realm, e.g. turn-dev-b.novasama-tech.org>
ICE_SERVERS=stun:<relay>:3478,turn:<relay>:3478?transport=udp,turns:<relay>:5349?transport=tcp

# notify-relay (stateless push relay; service-owned secrets)
NOTIFY_RATE_LIMIT=60                      # per-subject requests per window
NOTIFY_RATE_LIMIT_WINDOW_SECS=60
# iOS APNs: point APNS_KEY_FILE at the .p8 anywhere on the host. The file must
# exist before deploy and be readable by the container user (uid 10001):
# `sudo chown 10001:10001 <path> && sudo chmod 0400 <path>`. (If APNS_KEY_FILE is
# wrong, Docker creates a dir at that path — `sudo rmdir <path>`, fix it, redeploy.)
# Unset -> iOS pushes report provider failure.
APNS_KEY_FILE=/absolute/path/to/AuthKey_XXXX.p8
APNS_KEY_ID=<apns key id>
APNS_TEAM_ID=<apple team id>
APNS_TOPIC=<app bundle id>                # VoIP topic derives as <topic>.voip
APNS_ENVIRONMENT=<development|production>
FCM_SERVICE_ACCOUNT_JSON=<json or empty>  # Android; unset -> Android pushes report provider failure
```

The signer (or its proxy target) must be a registered attester authority with funds; the proxy relationship must already exist on-chain. Likewise each inviter (or the primary its `…PROXY_FOR` points at) must hold `AvailableInvites` on the `Game` / `ProofOfInk` pallets and funds for fees. A non-empty `JWT_JWKS_JSON` takes precedence over `JWT_ED25519_PUBLIC_KEY`.

The `notify-relay` provider secrets are optional: with none set the relay still boots and reports per-request provider failure for the unconfigured platform. `JWT_ED25519_PUBLIC_KEY` must be the public half of `identity-api`'s `JWT_ED25519_SECRET`.

The `turn-api` variables have compose dev defaults (placeholder secret, `example.org` realm, `example.com` ICE URLs), so leaving them unset boots a service that mints credentials **no real relay accepts**. `TURN_SECRET` is the base64 encoding of the relay's raw `static-auth-secret` string — base64-encoding a hex-decoded form produces different HMAC keys and the relay answers 401 to every Allocate.

## 3. Run

```bash
# the environment (first build takes several minutes; migrations run on boot)
sudo docker compose up -d --build

# the public edge — acquires its certificate on first start
sudo docker compose -f gateway/docker-compose.yml -p edge up -d

# monitoring — its own project, so an environment restart never stops it
sudo docker compose -f observability/docker-compose.yml -p observability up -d
```

Every process serves Prometheus metrics on port 9090 (`METRICS_ADDR`), published to no
host port and routed by no Caddy rule: they are reachable only from the `ibv2-metrics`
network, under the same `<service>-$ENV_ID` alias Prometheus scrapes in
[`observability/prometheus.yml`](../observability/prometheus.yml). Adding an environment
means copying that scrape job with the other `ENV_ID` suffix. The Prometheus UI is
loopback-only (`127.0.0.1:9091`) — reach it over an SSH tunnel, and set
`METRICS_ENABLED=false` to turn an environment's exporters off.

The route table lives in the repo: [`gateway/Caddyfile`](../gateway/Caddyfile) (see the
ownership map in `docs/plans/active/gateway.md`). Its `{$VAR:default}` defaults match this
layout exactly — domain `identity.dotspark.app`, upstreams on the `*-paseo-next-v2` container aliases,
docs at `/srv/docs` — so the edge starts with no environment at all. For a different domain,
set `GATEWAY_ADDRESS` in the edge project's environment rather than editing the file: CI
re-installs the file from the repo on every deploy.

**Rollback to a host-installed Caddy** (if the edge container is ever the problem): stop the
edge project, `apt-get install caddy`, `cp gateway/Caddyfile /etc/caddy/Caddyfile`, point its
upstream variables at loopback ports, bring the environment up with the debug overlay so those
ports exist, then `systemctl enable --now caddy`.

## 4. Verify

```bash
curl -fsS https://<domain>/readyz                          # {"chain":"up","db":"up",...}
curl -fsS https://<domain>/api/v1/attester
curl -fsS 'https://<domain>/api/v1/usernames/search?prefix=alice'   # routed to the indexer
curl -sS https://<domain>/api/v1/dim-ticket/x              # 401 problem+json = routed + auth gate live
curl -fsS https://<domain>/docs/ -o /dev/null              # committed API reference
sudo docker compose logs identity-chain-writer | tail -5   # "connected signer=…", "acquired writer lease"
sudo docker compose logs dim-tickets-writer | tail -5      # "connected signer=…", "acquired writer lease"
```

Confirm the host exposes nothing else:

```bash
sudo docker ps --format '{{.Names}}\t{{.Ports}}'    # only the edge container maps 80/443
ss -ltn                                             # no 8080–8085, no 5432–5435
```

Per-service `readyz` needs the debug overlay (loopback only):

```bash
sudo docker compose -f docker-compose.yml -f docker-compose.debug.yml up -d
curl -fsS http://127.0.0.1:8081/readyz   # projection freshness + dependencies
```

Failure isolation check (notify is independent): stop the relay and confirm auth/registration/search stay healthy, then bring it back.

```bash
sudo docker compose stop notify-relay
curl -fsS https://<domain>/readyz                                                  # still ready
curl -s -o /dev/null -w '%{http_code}\n' https://<domain>/api/v1/notify            # 502/504 only for notify
sudo docker compose start notify-relay                           # recovers without touching identity-api
```

For CI deploys to reach this server, set the repo secrets `IBV2_SSH_KEY` (private key authorized for `ubuntu@server`) and `IBV2_HOST` (server IP), and add a read-only GitHub deploy key on the server.

## One-time cutover from host Caddy

Only needed on a machine that still runs Caddy from `apt`. CI **skips** the edge
project until it exists, precisely so a merge cannot fight the host for 80/443.

```bash
cd ~/identity-backend-v2 && git pull

sudo docker network create ibv2-edge          # idempotent

# 1. recreate the services: they drop their published ports and join the network
sudo docker compose up -d --remove-orphans    # removes the old in-file gateway too

# 2. free 80/443 — this starts the downtime window
sudo systemctl disable --now caddy

# 3. the edge takes over and requests its certificate
sudo docker compose -f gateway/docker-compose.yml -p edge up -d --wait --wait-timeout 120

# 4. verify, then the window is closed
curl -fsS https://identity.dotspark.app/readyz
sudo docker ps --format '{{.Names}}\t{{.Ports}}'   # only the edge maps 80/443
```

Rollback: `sudo docker compose -f gateway/docker-compose.yml -p edge down`,
`sudo systemctl enable --now caddy`, and bring the services up with the debug
overlay so the loopback ports host Caddy proxies to exist again.

## Adding an environment

A second environment is another compose project on the same host with its own
`.env`, plus a DNS record and a site block. It shares the machine, the image,
and the edge; it shares no database, no signing key, and no chain.

1. **Check the chain is usable.** For PreviewNet: the shared attester authority
   `5F7H1LkZi8rnSH8PvUsp4LD2WkvcpnjkyQyvLPu1ntbhNr5T` must still list our writer
   signer as an `Any`/delay-0 proxy, hold an attestation allowance, and hold
   invites — see the post-wipe sequence in `docs/plans/active/previewnet-env.md`.
2. **Second checkout + `.env`** (mode 600) — one checkout per environment, each
   holding exactly one `.env`, which is what CI's deploy loop expects. Deliberately
   not one checkout with per-environment env files: compose auto-loads `./.env`
   whenever `--env-file` is absent, so a forgotten flag would run this project
   against the other environment's chain and signer without erroring. What differs
   from the first environment:

   ```bash
   ENV_ID=previewnet                                    # network aliases
   PEOPLE_RPC_URL=wss://previewnet.substrate.dev/people
   JWT_ED25519_SECRET=0x<fresh 64-hex>                  # its own issuer
   CHAIN_WRITER_PROXY_FOR=5F7H1LkZi8rnSH8PvUsp4LD2WkvcpnjkyQyvLPu1ntbhNr5T
   QUEUE_ENABLED=false
   DOTNS_GATEWAY_ENABLED=false                          # dotns is not deployed there
   ```

3. **Start only the services that environment needs**, as its own project:

   ```bash
   sudo docker compose -p identity-previewnet up -d \
     postgres identity-api identity-chain-writer \
     username-indexer-postgres username-indexer
   ```

   `invite-tickets` is deliberately excluded on PreviewNet: its inviter would have
   to sign as the same delegate the chain-writer uses, and two submitters on one
   account and chain race nonces. Its route falls through to that environment's
   `identity-api`, which 404s.

4. **DNS**: an A record for the hostname at the server IP, unproxied. The edge
   acquires the certificate on its next start once DNS resolves.
5. **Teach the shared services about the new issuer.** `turn-api` and
   `notify-relay` are shared across environments: set `JWT_JWKS_JSON` on them to
   a JWKS holding **both** environments' public keys. Tokens carry a `kid`
   derived from the signing key, so each is verified against the right one.
6. **Reload the edge** so it picks up the site block. The Caddyfile is a
   read-only bind mount, so a `git pull` already put the new block inside the
   running container: validate it in a throwaway container, then reload in
   place. This is what CI does, and it drops no connection — recreating the
   edge instead would interrupt **every** environment, since it is the only
   container bound to 80/443. The certificate for the new hostname is acquired
   in the background once DNS resolves.

   ```bash
   sudo docker run --rm -v "$PWD/gateway/Caddyfile:/etc/caddy/Caddyfile:ro" \
     caddy:2-alpine caddy validate --config /etc/caddy/Caddyfile
   sudo docker compose -f gateway/docker-compose.yml -p edge \
     exec -T caddy caddy reload --config /etc/caddy/Caddyfile
   ```

   A reload only re-reads the file. Changing the edge container's *environment*
   or ports — a new `{$VAR}` placeholder in the Caddyfile, which must also be
   added to the `environment:` pass-through list in `gateway/docker-compose.yml`
   — needs `up -d` (add `--force-recreate` only if compose reports no change).
   Write a new environment's hostname and upstreams as literals to stay on the
   reload path.

7. **Verify** the new hostname serves `/readyz`, `/api/v1/attester`, and search;
   that the original hostname is unchanged; and that a token from one
   environment is rejected by the other.

## Release images

Pushing a `v*` tag runs [`release.yml`](../.github/workflows/release.yml), publishing a
multi-arch (`linux/amd64` + `linux/arm64`) image to
`ghcr.io/paritytech/identity-backend-rust` tagged `1.2.3` and `v1.2.3`. Each architecture
builds on its own native runner and the two are joined into one manifest list, so a single
tag serves both. Releases are independent of the `main` deploy above: tagging never
restarts production, and `latest` keeps tracking `main`. Tag a commit that already passed
CI on `main` — the release workflow builds and publishes without re-running the test gate.

```bash
git tag v0.1.0 && git push origin v0.1.0
```

The package inherits the repo's private visibility, so external consumers need a GitHub
token with `read:packages` (SSO-authorized for `paritytech`) to pull. Make the package
public only as a deliberate decision — it publishes binaries of a private repo.

To run a release image rather than build one, set `IDENTITY_IMAGE` to the tag and pass
`--no-build`; otherwise compose rebuilds from source, because every application service
declares `build: .`. The services each fail fast on missing configuration:

| Service | Required |
|---|---|
| `identity-api` | `DATABASE_URL`, `PEOPLE_RPC_URL`, `JWT_ED25519_SECRET`, `ATTESTER_PUBLIC_KEY` |
| `identity-chain-writer` | `DATABASE_URL`, `PEOPLE_RPC_URL`, `CHAIN_WRITER_SIGNER_SURI` |
| `username-indexer` | `DATABASE_URL`, `PEOPLE_RPC_URL` |

`identity-api` and `username-indexer` need **separate databases**: each applies its own
`sqlx::migrate!` set and they would collide on a shared `_sqlx_migrations` table. Dev-only
defaults for every variable are in `.env.example`.

## Troubleshooting

| You see | Fix |
|---|---|
| `migration N was previously applied but has been modified` | Volume holds an older schema. `sudo docker compose down && sudo docker volume rm identity-backend-v2_pgdata && sudo docker compose up -d` — destroys DB state, back up first ([runbook](runbook.md#database-backup-manual)). |
| writer: `Cannot parse hex string` | Malformed `CHAIN_WRITER_SIGNER_SURI` (see accepted formats above). |
| `identity-api` never healthy | It blocks on the People Chain RPC at startup — check connectivity to `PEOPLE_RPC_URL`. |
| `dim-tickets-api` never healthy | Same startup shape as `identity-api` (Postgres + People RPC); also requires `INVITER_ADDRESS` + JWT public-key material — a missing var aborts boot loudly. |
| dim tickets stuck `PENDING` | `dim-tickets-writer` down or inviter out of `AvailableInvites` — `sudo docker compose logs dim-tickets-writer`; the read path keeps serving either way. |
| `username-indexer` never healthy | Check its Postgres, People Chain RPC, and bootstrap logs with `sudo docker compose logs username-indexer`. |
| No TLS certificate | DNS not propagated, 80/443 blocked, or a host Caddy still holding the ports. `dig +short <domain>`, `sudo ss -ltnp '( sport = :443 )'`, `sudo docker compose -f gateway/docker-compose.yml -p edge logs caddy`. |
| Edge answers 502 | The upstream alias does not resolve: the environment project is down, or its `ENV_ID` does not match the Caddyfile's upstream suffix. `sudo docker network inspect ibv2-edge`. |
| `network ibv2-edge not found` | Created out of band, once per host: `sudo docker network create ibv2-edge`. |
| `/docs` returns 404 | The edge mounts `../docs/api-reference` from the checkout — confirm the path exists and re-create the edge project after a `git pull`. |
