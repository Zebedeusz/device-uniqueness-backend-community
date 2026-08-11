# Runbook

Operating the running deployment. From-zero setup: [deploy.md](deploy.md).

Test deployment: `https://identity.dotspark.app` on `ubuntu@51.15.134.8`, repo at `~/identity-backend-v2`.

Two compose projects share the host: the environment (the project name declared in `docker-compose.yml` (`identity-backend-v2`), from `docker-compose.yml`) and the public edge (`-p edge`, from `gateway/docker-compose.yml`). **Every command below needs its project**, or it will act on the wrong stack — or on none. The edge is the only container publishing a port; service ports exist locally only under the debug overlay.

## Deploys

Push to `main` → CI tests, builds `ghcr.io/paritytech/identity-backend-rust:<sha>`, and the server pulls + restarts (health-gated). Watch with `gh run watch`.

Manual restart, on the image CI already built for this checkout:

```bash
cd ~/identity-backend-v2 && git pull
sudo IDENTITY_IMAGE=ghcr.io/paritytech/identity-backend-rust:$(git rev-parse HEAD) \
  docker compose up -d --no-build
# Image not cached locally? The server holds no ghcr credentials:
#   echo <PAT with read:packages> | sudo docker login ghcr.io -u <user> --password-stdin
# Last resort, builds on the server (several minutes):
#   sudo docker compose up -d --build
# only when the Caddyfile changed — a reload, not a restart (see Caddy / TLS / docs)
sudo docker compose -f gateway/docker-compose.yml -p edge \
  exec -T caddy caddy reload --config /etc/caddy/Caddyfile
```

Recreate the **whole project**, never a single service — `up -d <service>` on its own
leaves the rest on the previous image, and a stack split across two builds fails in
ways neither build does (an old writer reading config the new compose no longer passes).

Migrations run automatically on boot — never by hand. Never overwrite the server's `.env`.

## Health and logs

```bash
curl -fsS https://identity.dotspark.app/readyz                    # gates on Postgres + People Chain RPC
curl -fsS 'https://identity.dotspark.app/api/v1/usernames/search?prefix=a'   # the indexer, through the edge
sudo docker compose ps
sudo docker compose logs -f identity-api
sudo docker compose logs -f identity-chain-writer
sudo docker compose logs -f username-indexer
sudo docker compose -f gateway/docker-compose.yml -p edge logs -f caddy
```

Per-service `readyz` from the host needs the debug overlay, which republishes the ports on
loopback only. Layer it on, poke, then drop back:

```bash
sudo docker compose -f docker-compose.yml -f docker-compose.debug.yml up -d
curl -fsS http://127.0.0.1:8081/readyz     # index freshness + Postgres + People Chain RPC
sudo docker compose up -d --remove-orphans   # back to no published ports
```

Healthy writer startup: `connected signer=0x…` then `acquired writer lease`.

## Registration outbox

`username_reservations` is the source of truth (`RESERVED → SUBMITTING → ASSIGNED | RETRY_AFTER | FAILED_TERMINAL`); the chain is reconciled to it.

```bash
sudo docker compose exec -T postgres psql -U identity -d identity \
  -c "select id, full_username, status, attempt, tx_hash, left(last_error,120) as err
      from username_reservations order by id desc limit 10;"
```

- `RETRY_AFTER` retries automatically (max 8 attempts); `FAILED_TERMINAL` never retries — inspect `last_error`.
- Independent check: query `Resources.UsernameOwnerOf("<base>.<NN>")` on the People Chain.

## dotNS gateway lane (Asset Hub)

A second, independent state machine on the same row. `status` is the People
registration. `dotns_status` is the Asset Hub reservation. **A dotNS failure never
changes `status`.** `ASSIGNED` + `DOTNS_FAILED_TERMINAL` means the username works
and the dotNS name does not. **On by default** (`DOTNS_GATEWAY_ENABLED`), because
the `PEOPLE_RPC_URL` and `ASSET_HUB_RPC_URL` defaults are a matched PreviewNet
pair and PreviewNet's Asset Hub is the only one running the `signed_at`
`reserve_name`. The two URLs must always name the same network — a split pair
claims labels on the wrong chain, unrecoverably. `paseo-next-v2` therefore
carries an explicit `DOTNS_GATEWAY_ENABLED=false`, pending its Asset Hub runtime
upgrade; if that line is ever missing from its `.env`, the lane is live there and
the fix is to add it and redeploy.

```bash
sudo docker compose exec -T postgres psql -U identity -d identity \
  -c "select id, full_username, status, dotns_status, dotns_attempt, dotns_tx_hash,
             left(dotns_last_error,120) as err
      from username_reservations where dotns_status is not null
      order by id desc limit 10;"
```

- `dotns_status` `NULL` = the request carried no `dotns` block, or the row predates
  the lane. There is no backfill; those are never submitted.
- `EXPIRED` is **not** a bug to retry. `reserve_name` enforces a 3-day window on the
  client's `signedAt`, and **the backend cannot re-sign** — only the client holds the
  candidate key. It means the row sat unsubmitted for days: writer down, or parked
  in `QUEUED`. The client must re-register to get a dotNS name.
- `FAILED_TERMINAL` — inspect `dotns_last_error`. Common causes: `dotns signature
  does not verify`, meaning the client bound it to a different attester than
  `GET /api/v1/attester` returns; `lite label reserved by another account`; or a
  contract revert.
- `ABANDONED` — the **People** half reached `FAILED_TERMINAL`, so this half was
  never attempted. Nothing is wrong with the reservation; there is no username to
  attach a name to. Diagnose the People `last_error`, not `dotns_last_error`. The
  lane closes itself in the same statement that fails the People row, because
  `claim_dotns_due` only takes rows already `ASSIGNED` — a row left `PENDING` here
  would be unclaimable forever while still inflating the `PENDING` gauges below.
- Independent check: query `DotnsGateway.LiteLabelOwner("<base>.<NN>")` on Asset Hub.
- `RETRY_AFTER` with a `dotns_last_error` about a **future-dated** signature is not
  a failure: the client's `signedAt` is further ahead of the chain's clock than
  `MaxFutureSkewSeconds` allows. It clears itself once `dotns_not_before` passes,
  and costs no attempt. A steady stream of these means client clock skew.
- Watch `ibv2_dotns_lane_connected` (`1` up, `0` parked), `ibv2_dotns_outbox_depth{status}`,
  `ibv2_dotns_attester_allowance`, and
  `ibv2_account_free_balance_planck{role="signer",chain="asset-hub"}`. A parked lane
  with a rising `PENDING` depth is the expected shape while Asset Hub is down; a
  connected lane with a rising `PENDING` depth is not. The allowance
  is a **second budget**, separate from People's. Either hitting zero stops
  registration in its own half. `reserve_name` is fee-free only when it succeeds, so
  a run of failures drains the signer's Asset Hub balance.
- `ibv2_account_free_balance_planck` carries `chain` on **every** series, People
  included. A selector written as `{role="signer"}` alone matches both chains —
  always pin `chain="people"` or `chain="asset-hub"` in alerts and dashboards.

## Writer rules

- **Exactly one instance.** A nonce lane is (signing account, chain). Never
  `--scale identity-chain-writer`. It holds two lanes on one account, People and
  Asset Hub. That is safe because nonces are per chain, and both sit behind the
  same single lease.
- Restart-safe: reconciles `SUBMITTING` rows on both chains against chain state
  instead of resubmitting.
- **One dotNS condition refuses to start:** `DOTNS_GATEWAY_ENABLED` on with no
  `ASSET_HUB_RPC_URL`. That is a config error — identity-api would accept blocks
  nothing submits — and is not restart-fixable; correct the `.env`.
  The attester identity needs no separate check: `ATTESTER_ACCOUNT` names the
  authority and proxying is derived from it, so the writer's signing origin
  equals the published attester on both chains by construction.
- **Everything else dotNS parks the lane, not the writer.** Asset Hub is dialled
  on the first pass rather than at boot, and re-dialled every 30s while down, so
  an unreachable endpoint or a `reserve_name` shape mismatch (Paseo next still
  runs the older proof-of-ownership variant) leaves rows in `PENDING` and keeps
  People registrations flowing. Watch `ibv2_dotns_lane_connected`: `1` = up,
  `0` = parked, absent = lane disabled. The parking `warn!` names the cause and
  is logged once per distinct cause, not once per pass — check the first
  occurrence, not the latest.
- Restart alone: `sudo docker compose restart identity-chain-writer`. `restart` reuses the
  existing container, so it never changes the image; a version change is a project-wide
  `up -d` (see [Deploys](#deploys)).

## Config / secrets

- Everything is in `~/identity-backend-v2/.env` (mode 600); apply changes with `sudo docker compose up -d`.
- Rotating `JWT_ED25519_SECRET` invalidates outstanding JWTs (clients re-authenticate).
- A new writer key must be an authorized attester (or proxy) with funds.
- Rotating `TURN_SECRET` invalidates outstanding TURN credentials (≤ `TURN_TTL_SECS` old): update
  the relay (coturn `--use-auth-secret`) and `turn-api` together, then `sudo docker compose up -d`.
- Rotating `POC_HMAC_SECRET` invalidates outstanding puzzles (≤ 90s old): in-flight solvers get
  `402` "checksum does not match" and simply request a new puzzle. Safe to rotate at any time.

## Database backup (manual)

No automatic backups (testnet). Before anything risky:

```bash
sudo docker compose exec -T postgres pg_dump -U identity identity | gzip > ~/identity-$(date +%Y%m%d-%H%M%S).sql.gz
# restore into an empty DB:
gunzip -c ~/identity-<timestamp>.sql.gz | sudo docker compose exec -T postgres psql -U identity -d identity
```

The edge's `edge_caddy_data` volume holds the TLS certificates and the ACME account key. Losing
it is not fatal (Caddy re-issues) but repeated loss burns Let's Encrypt duplicate-certificate
allowance, so include it when backing the host up:

```bash
sudo docker run --rm -v edge_caddy_data:/data -v ~:/backup alpine \
  tar czf /backup/caddy-data-$(date +%Y%m%d).tgz -C /data .
```

## Caddy / TLS / docs

- The route table is the **committed** [`gateway/Caddyfile`](../gateway/Caddyfile) (ownership
  map: `docs/plans/active/gateway.md`), mounted read-only into the edge container. Never edit a
  running copy — change the repo file, `git pull`, then validate and reload in place. The mount
  means the new file is already inside the container, so this drops no connection; recreating the
  edge would interrupt **every** environment, since it is the only container on 80/443.

  ```bash
  sudo docker run --rm -v "$PWD/gateway/Caddyfile:/etc/caddy/Caddyfile:ro" \
    caddy:2-alpine caddy validate --config /etc/caddy/Caddyfile
  sudo docker compose -f gateway/docker-compose.yml -p edge \
    exec -T caddy caddy reload --config /etc/caddy/Caddyfile
  ```

  A reload only re-reads the file. Changing the container's *environment* or ports — a new
  `{$VAR}` placeholder, which must also join the `environment:` pass-through list in
  `gateway/docker-compose.yml` — needs `up -d` and accepts that brief interruption.
- Migrated routes go to their service's container alias (`username-indexer-paseo-next-v2:8080`, …);
  everything unhandled goes to `identity-api-<ENV_ID>`. `/api/v1/poc*` goes to the indexer
  alongside the username reads it gates.
- Certificates live in the `edge_caddy_data` volume and renew automatically.
  `https://<domain>/docs` serves `docs/api-reference/` mounted from the checkout — a `git pull`
  updates the files, but recreate the edge project if the mount looks stale.
- The edge is the only container publishing a port. If anything else appears in
  `sudo docker ps --format '{{.Ports}}'`, someone left the debug overlay applied.

## Proof-of-compute gate (public search)

Off by default. When on, `GET /api/v1/usernames/search` needs **either** a valid identity-service
JWT **or** a solved puzzle from `POST /api/v1/poc/issue`; both shipping mobile apps authenticate
search, so only anonymous callers (the Desktop/host path, and Android's `DISABLE_AUTH` nightly
build) have to mine. An unverifiable bearer is treated as anonymous — this route never returns 401.

Enable (username-indexer only):

```bash
# in ~/identity-backend-v2/.env
POC_ENABLED=true
POC_HMAC_SECRET=<random, >=32 chars>   # required when enabled; boot fails without it
sudo docker compose up -d username-indexer
```

The service also needs verify-only JWT material (`JWT_JWKS_JSON` or `JWT_ED25519_PUBLIC_KEY`) or it
refuses to boot — without it every caller would be anonymous and the shipping apps would be forced
to mine. Disable by setting `POC_ENABLED=false` and recreating the service: the issuance route
unmounts and search returns to unconditional public reads.

Smoke it:

```bash
curl -sX POST https://<domain>/api/v1/poc/issue                       # 201 + puzzle
curl -so /dev/null -w '%{http_code}\n' 'https://<domain>/api/v1/usernames/search?prefix=a'  # 402
```

Difficulty tuning: `POC_DIFFICULTY_BITS` (1–32, default 16) is the number of leading zero bits, so
each extra bit doubles the expected work. 16 bits is ~0.1s of native single-threaded mining; raise
it only with a measurement from the slowest expected client.

## Failure modes

| Symptom | Action |
|---|---|
| `readyz`: `chain: down` | People Chain RPC unreachable/changed — check `PEOPLE_RPC_URL`. |
| Claims returning 422 `Pool exhausted` | The ticket pool drained: check `invite-tickets-pool` logs (`ticket batch finalized … registered=0` = inviter out of `AvailableInvites` quota or unauthorized; `pool tick failed` = RPC/signer trouble). Pool size is logged each tick — treat sustained `available < ~10% of POOL_TARGET_SIZE` as the alert threshold; claims keep serving until depletion. |
| `invite-tickets-pool` logs `another maintainer instance holds the pool lock` | A second replica or a stuck deploy overlap — scale back to exactly one. |
| Writer logs `queue advancer is down with claims queued; holding the throttle` | The registration queue is enabled but the `registration-queue` service is dead (lease stale/absent), so free-lane claims are parking as `QUEUED` and nothing is draining — this is deliberate: the queue is the free lane's throughput control and a dead queue never falls back to unthrottled direct registration. Restart it (`sudo docker compose restart registration-queue`); the queue resumes fair draining on its own. To retire the queue instead, set `QUEUE_ENABLED=false` for **both** identity-api and identity-chain-writer (writer last) — the writer then drains the leftovers after `QUEUE_FALLBACK_AFTER_SECS`. Treat a warning that persists past one restart as a page. |
| Queue behaving oddly: `QUEUED` rows draining with the advancer down, or a stranded-queue warning while intake goes direct | `QUEUE_ENABLED` is split between identity-api and identity-chain-writer (writer off + api on = the janitor silently drains a queue the api is still filling — the throttle is gone; writer on + api off = warnings about leftovers no new claim joins). The values must match; `scripts/verify_compose_boundaries.sh` pins both wires. |
| Rows stuck in `RETRY_AFTER` with wasm-trap errors | Invalid payload for `PeopleLite.attest`, or attester/proxy authorization missing on-chain. |
| Any lane logging `The extrinsic payload is not compatible with the live chain` | The runtime changed shapes under the vendored metadata (a wipe or upgrade). Refresh `crates/chain-types/metadata/people.scale` with the `subxt metadata` command in the `chain-types` crate docs, `subxt diff` the old and new blobs to see what moved, then rebuild and deploy. |
| Every extrinsic failing with `Transaction has a bad signature`, nonce back at 0 | The chain was reset: the process still holds the old genesis hash, captured when its client connected. Restart the service — reconnecting alone does not re-read it. Then re-check funding, proxy delegation and attestation allowance, which the reset also cleared. |
| Writer exits at boot | Bad `CHAIN_WRITER_SIGNER_SURI` or DB unreachable. |
| Username search fails while identity routes work | Check the indexer's logs and its Caddy route; for a direct `readyz`, layer the debug overlay and curl `127.0.0.1:8081`. |
| TURN 201s minted but relay rejects the credentials | `TURN_SECRET`/`TURN_AUTH_ALGORITHM` drifted from the relay's coturn config — they must match exactly; rotate both together. `turn-api` itself is stateless (readyz never gates on anything). |
| Public search suddenly returning `402` | The proof-of-compute gate is on (`POC_ENABLED`). Expected for anonymous callers; if authenticated clients see it, the indexer's verify-only JWT material is wrong — compare `JWT_ED25519_PUBLIC_KEY`/`JWT_JWKS_JSON` against identity-api's `/.well-known/jwks.json`. |
| `402` "puzzle has already been used" on a first attempt | The client is reusing a puzzle (one solve = one request) or a proxy is retrying. Each request needs a fresh `POST /api/v1/poc/issue`. |
| `spent_puzzles` growing without bound | The pruner logs `pruning expired spent puzzles failed`; check indexer logs and DB health. Rows are harmless but unbounded until it recovers. |
| Down after reboot | Shouldn't happen (`restart: unless-stopped`); if it does: `sudo docker compose up -d` and `sudo docker compose -f gateway/docker-compose.yml -p edge up -d`. |
| Edge answers 502 for everything | The environment project is down, or its `ENV_ID` no longer matches the Caddyfile upstream suffix: `sudo docker network inspect ibv2-edge` and compare the aliases. |
