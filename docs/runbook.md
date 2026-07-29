# Runbook

Operating the running deployment. From-zero setup: [deploy.md](deploy.md).

Test deployment: `https://identity.dotspark.app` on `ubuntu@51.15.134.8`, repo at `~/identity-backend-v2`.

## Deploys

Push to `main` → CI tests, builds `ghcr.io/paritytech/identity-backend-rust:<sha>`, and the server pulls + restarts (health-gated). Watch with `gh run watch`.

Manual fallback (builds on the server):

```bash
cd ~/identity-backend-v2 && git pull && sudo docker compose up -d --build
```

Migrations run automatically on boot — never by hand. Never overwrite the server's `.env`.

## Health and logs

```bash
curl -fsS http://127.0.0.1:8080/readyz     # gates on Postgres + People Chain RPC
curl -fsS http://127.0.0.1:8081/readyz     # index freshness + Postgres + People Chain RPC
sudo docker compose ps
sudo docker compose logs -f identity-api
sudo docker compose logs -f identity-chain-writer
sudo docker compose logs -f username-indexer
journalctl -u caddy -f
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

## Writer rules

- **Exactly one instance.** One signer = one nonce lane; never `--scale identity-chain-writer`.
- Restart-safe: reconciles `SUBMITTING` rows against chain state instead of resubmitting.
- Restart alone: `sudo docker compose restart identity-chain-writer`.

## Config / secrets

- Everything is in `~/identity-backend-v2/.env` (mode 600); apply changes with `sudo docker compose up -d`.
- Rotating `JWT_ED25519_SECRET` invalidates outstanding JWTs (clients re-authenticate).
- A new writer key must be an authorized attester (or proxy) with funds.
- Rotating `TURN_SECRET` invalidates outstanding TURN credentials (≤ `TURN_TTL_SECS` old): update
  the relay (coturn `--use-auth-secret`) and `turn-api` together, then `sudo docker compose up -d`.
- Rotating `POC_HMAC_SECRET` invalidates outstanding puzzles (≤ `POC_SESSION_TTL_SECS +
  POC_CLOCK_SKEW_SECS` old): in-flight solvers get `402` "checksum does not match" and simply
  request a new puzzle. Safe to rotate at any time.

## Database backup (manual)

No automatic backups (testnet). Before anything risky:

```bash
sudo docker compose exec -T postgres pg_dump -U identity identity | gzip > ~/identity-$(date +%Y%m%d-%H%M%S).sql.gz
# restore into an empty DB:
gunzip -c ~/identity-<timestamp>.sql.gz | sudo docker compose exec -T postgres psql -U identity -d identity
```

## Caddy / TLS / docs

- The route table is the **committed** [`gateway/Caddyfile`](../gateway/Caddyfile) (ownership
  map: `docs/plans/active/gateway.md`); CI validates and re-installs it into
  `/etc/caddy/Caddyfile` on every deploy — never hand-edit the installed copy, change the repo
  file. Manual apply: `sudo cp gateway/Caddyfile /etc/caddy/Caddyfile && sudo caddy validate
  --config /etc/caddy/Caddyfile && sudo systemctl reload caddy`.
- Migrated routes go to their service on `127.0.0.1:8081–8085`; everything unhandled goes to
  `identity-api` on `127.0.0.1:8080`; unmigrated legacy routes forward to
  `LEGACY_BACKEND_ORIGIN` when configured (cutover lane). `/api/v1/poc*` goes to the indexer
  (:8081) alongside the username reads it gates.
- Certificates renew automatically. `https://<domain>/docs` serves `docs/api-reference/` from the checkout — a `git pull` updates it, no rebuild.

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
| Rows stuck in `RETRY_AFTER` with wasm-trap errors | Invalid payload for `PeopleLite.attest`, or attester/proxy authorization missing on-chain. |
| Writer exits at boot | Bad `CHAIN_WRITER_SIGNER_SURI` or DB unreachable. |
| Username search fails while identity routes work | Check `curl -fsS http://127.0.0.1:8081/readyz`, indexer logs, and the dedicated Caddy route. |
| TURN 201s minted but relay rejects the credentials | `TURN_SECRET`/`TURN_AUTH_ALGORITHM` drifted from the relay's coturn config — they must match exactly; rotate both together. `turn-api` itself is stateless (readyz never gates on anything). |
| Public search suddenly returning `402` | The proof-of-compute gate is on (`POC_ENABLED`). Expected for anonymous callers; if authenticated clients see it, the indexer's verify-only JWT material is wrong — compare `JWT_ED25519_PUBLIC_KEY`/`JWT_JWKS_JSON` against identity-api's `/.well-known/jwks.json`. |
| `402` "puzzle has already been used" on a first attempt | The client is reusing a puzzle (one solve = one request) or a proxy is retrying. Each request needs a fresh `POST /api/v1/poc/issue`. |
| `spent_puzzles` growing without bound | The pruner logs `pruning expired spent puzzles failed`; check indexer logs and DB health. Rows are harmless but unbounded until it recovers. |
| Down after reboot | Shouldn't happen (`restart: unless-stopped`); if it does: `sudo docker compose up -d`. |
