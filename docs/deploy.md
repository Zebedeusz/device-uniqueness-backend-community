# Deploy

From-zero setup of a server. Day-2 operations: [runbook.md](runbook.md).

Topology: `identity-api`, `identity-chain-writer`, and the single-instance `registration-queue` advancer (optional at runtime: no signing secret, and when it is down api + writer fall back to direct registration) share the identity Postgres; `username-indexer` owns a separate projection Postgres; `dim-tickets-api` and `dim-tickets-writer` share a third, dim-tickets-owned Postgres; `invite-tickets-api` and the single-instance `invite-tickets-pool` share a fourth, invite-tickets-owned Postgres (the ticket pool holds private keys — nothing shared). `turn-api` and `notify-relay` are stateless (no Postgres). Caddy routes the public API between the six HTTP services (route table: the committed [`gateway/Caddyfile`](../gateway/Caddyfile)) and provides automatic HTTPS. Normal updates are done by CI ([workflow](../.github/workflows/deploy.yml)) — this page is only for standing up a new machine.

Secret boundaries: the JWT **signing** seed lives only in `identity-api`; `dim-tickets-api`, `invite-tickets-api`, `turn-api`, and `notify-relay` get the **public** key (or JWKS) only. Each chain-submitting worker holds only its own signing secret: the dim inviter SURI lives only in `dim-tickets-writer`, the invite inviter SURI only in `invite-tickets-pool`. `turn-api` additionally holds `TURN_SECRET` — the HMAC key shared with the TURN relay (coturn `--use-auth-secret`); relay and issuer must be rotated together, and no other service sees it. Push credentials (`APNS_*`/`FCM_*`) live only in `notify-relay`. Compose uses the root `.env` for interpolation only and gives every application container an explicit environment allowlist, so no service can read another's key.

**Nonce lanes**: `dim-tickets-writer` and `invite-tickets-pool` each submit as "an inviter" and MUST NOT sign as the same account — two independent submitters on one account race nonces. Give each its own account (e.g. separate proxy delegates of one cold inviter primary); compose interpolates them from distinct `INVITER_*` (dim) and `INVITE_INVITER_*` (invite) host variables.

Postgres is reachable by sibling containers on the internal Compose network. The optional dim-tickets host port is bound to `127.0.0.1` only; set a URL-safe random `DIM_TICKETS_POSTGRES_PASSWORD` on deployments.

## Prerequisites

- Ubuntu server with sudo and outbound internet (services connect to a People Chain RPC at startup)
- DNS A record pointing the domain at the server IP (direct, not proxied)
- Ports 80/443 open; the attester signing key (ask the IB team)

## 1. Install Docker and Caddy

```bash
sudo apt-get update
sudo apt-get install -y docker.io docker-compose-v2 ca-certificates curl gnupg
sudo systemctl enable --now docker

curl -1sLf "https://dl.cloudsmith.io/public/caddy/stable/gpg.key" | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf "https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt" | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt-get update && sudo apt-get install -y caddy
```

## 2. Code and config

```bash
git clone git@github.com:paritytech/identity-backend-rust.git ~/identity-backend-v2
cd ~/identity-backend-v2
```

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

## 3. Run

```bash
sudo docker compose up -d --build      # first build takes several minutes; migrations run on boot
```

The route table lives in the repo: [`gateway/Caddyfile`](../gateway/Caddyfile) (see the
ownership map in `docs/plans/active/gateway.md`). Its `{$VAR:default}` defaults match this
layout exactly — domain `identity.dotspark.app`, upstreams on `127.0.0.1:8080–8085`, docs under
`/home/ubuntu/identity-backend-v2` — so installing it needs no environment. For a different
domain or paths, set the variables via a systemd drop-in for `caddy.service` instead of editing
the file (edits would be overwritten: CI re-installs the file on every deploy).

```bash
sudo chmod o+x /home/ubuntu
sudo cp ~/identity-backend-v2/gateway/Caddyfile /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile && sudo systemctl reload caddy
```

Unmigrated legacy routes (`/api/v1/poc|schemas|subscriptions`, `/admin`, `/debug`)
forward to `{$LEGACY_BACKEND_ORIGIN}` when set (route-by-route cutover — point it at the
deployed legacy backend); unset, they fall through to `identity-api`'s 404.

## 4. Verify

```bash
curl -fsS http://127.0.0.1:8080/readyz          # {"chain":"up","db":"up",...}
curl -fsS http://127.0.0.1:8081/readyz          # projection freshness + dependencies
curl -fsS http://127.0.0.1:8082/readyz          # dim-tickets: {"chain":"up","db":"up",...}
curl -fsS http://127.0.0.1:8084/readyz          # turn: {"service":"turn","status":"ready"} (stateless)
curl -fsS http://127.0.0.1:8085/readyz          # notify-relay: {"status":"ready",...} (no external deps)
curl -fsS https://<domain>/api/v1/attester
curl -fsS 'https://<domain>/api/v1/usernames/search?prefix=alice'
curl -sS https://<domain>/api/v1/dim-ticket/x   # 401 problem+json = routed + auth gate live
sudo docker compose logs identity-chain-writer | tail -5   # "connected signer=…", "acquired writer lease"
sudo docker compose logs dim-tickets-writer | tail -5      # "connected signer=…", "acquired writer lease"
```

Failure isolation check (notify is independent): stop the relay and confirm auth/registration/search stay healthy, then bring it back.

```bash
sudo docker compose stop notify-relay
curl -fsS http://127.0.0.1:8080/readyz && curl -fsS http://127.0.0.1:8081/readyz   # still ready
curl -s -o /dev/null -w '%{http_code}\n' https://<domain>/api/v1/notify            # 502/504 only for notify
sudo docker compose start notify-relay                                             # recovers without touching identity-api
```

For CI deploys to reach this server, set the repo secrets `IBV2_SSH_KEY` (private key authorized for `ubuntu@server`) and `IBV2_HOST` (server IP), and add a read-only GitHub deploy key on the server.

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
| No TLS certificate | DNS not propagated or 80/443 blocked. `dig +short <domain>`, `journalctl -u caddy`. |
| `/docs` returns 403/404 | Caddy can't traverse `/home/ubuntu` — re-run the `chmod o+x` line. |
