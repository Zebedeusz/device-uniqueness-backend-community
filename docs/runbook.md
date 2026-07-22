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
- Migrated routes go to their service on `127.0.0.1:8081–8084`; everything unhandled goes to
  `identity-api` on `127.0.0.1:8080`; unmigrated legacy routes forward to
  `LEGACY_BACKEND_ORIGIN` when configured (cutover lane).
- Certificates renew automatically. `https://<domain>/docs` serves `docs/api-reference/` from the checkout — a `git pull` updates it, no rebuild.

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
| Down after reboot | Shouldn't happen (`restart: unless-stopped`); if it does: `sudo docker compose up -d`. |
