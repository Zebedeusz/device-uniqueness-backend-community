# Runbook — Identity Backend V2

Day-2 operations for a running deployment. For standing up from zero, see [DEPLOY.md](DEPLOY.md).

Current test deployment: `https://identity.dotspark.app` on `ubuntu@51.15.134.8` (repo at `~/identity-backend-v2`).

## Update / redeploy

Preferred — one command from a local checkout (backs up the DB, syncs code, rebuilds, waits for `/readyz`):

```bash
just deploy          # wraps scripts/deploy.sh; DEPLOY_HOST=ibv2 by default
```

It never touches the server's `.env`, and it takes a `pg_dump` to `~/backups/` on the server before anything else (keeps the last 14).

Manual alternative on the server:

```bash
cd ~/identity-backend-v2
git pull
sudo docker compose up -d --build
```

Only changed layers rebuild. Migrations apply automatically on boot (advisory-locked, safe across replicas). Never run migrations by hand.

Never sync or overwrite the server's `.env`.

## Backups / restore

`just deploy` and CI both write `~/backups/identity-<timestamp>.sql.gz` on the server before each deploy. Manual backup:

```bash
sudo docker compose exec -T postgres pg_dump -U identity identity | gzip > ~/backups/identity-$(date +%Y%m%d-%H%M%S).sql.gz
```

Restore into a fresh/empty database (e.g. after a volume wipe, once containers are up):

```bash
gunzip -c ~/backups/identity-<timestamp>.sql.gz | sudo docker compose exec -T postgres psql -U identity -d identity
```

## Health

```bash
curl -fsS http://127.0.0.1:8080/livez        # process up
curl -fsS http://127.0.0.1:8080/readyz      # gates on live Postgres + People Chain RPC
sudo docker compose ps
```

## Logs

```bash
cd ~/identity-backend-v2
sudo docker compose logs -f identity-api
sudo docker compose logs -f identity-chain-writer
journalctl -u caddy -f
```

Healthy writer startup looks like:

```
identity-chain-writer connected signer=0x…
acquired writer lease epoch=N
```

## Registration outbox

The `username_reservations` table is the source of truth (`RESERVED → SUBMITTING → ASSIGNED | RETRY_AFTER | FAILED_TERMINAL`); the chain is reconciled to it.

```bash
sudo docker compose exec -T postgres psql -U identity -d identity \
  -c "select id, full_username, status, attempt, tx_hash, left(last_error,120) as err
      from username_reservations order by id desc limit 10;"
```

- `RETRY_AFTER` rows are retried automatically with backoff (max 8 attempts).
- `FAILED_TERMINAL` rows are never retried; inspect `last_error`.
- Confirm on-chain ownership independently: query `Resources.UsernameOwnerOf("<base>.<NN>")` on the People Chain (e.g. with the `dot` CLI).

## Writer rules

- **Exactly one instance, always.** One signer = one nonce lane; the `writer_lease` table only guards deploy overlap. Never `--scale identity-chain-writer`.
- Restart safety: on start it reconciles `SUBMITTING` rows against chain state instead of resubmitting.
- Restart it alone: `sudo docker compose restart identity-chain-writer`.

## Config / secrets

- Everything lives in `~/identity-backend-v2/.env` (mode 600). Var meanings: see DEPLOY.md step 3.
- After editing `.env`: `sudo docker compose up -d` (recreates changed containers).
- Rotating `JWT_ED25519_SECRET` invalidates all outstanding JWTs (clients re-authenticate; refresh tokens re-mint against the new key on use).
- Changing the writer key requires the new account (or its proxy target) to be an authorized attester with funds.

## Caddy / TLS / docs

- Config: `/etc/caddy/Caddyfile`; apply with `sudo caddy validate --config /etc/caddy/Caddyfile && sudo systemctl reload caddy`.
- Certificates renew automatically; state in `/var/lib/caddy/`.
- `https://<domain>/docs` serves `docs/api-reference/index.html` straight from the repo checkout — updating docs is just a `git pull`/rsync, no rebuild.

## Known failure modes

| Symptom | Cause / action |
|---|---|
| `readyz` says `chain: down` | People Chain RPC unreachable or endpoint changed. Check `PEOPLE_RPC_URL` against the current environments list. |
| Registrations stuck in `RETRY_AFTER` with wasm-trap errors | Payload invalid for `PeopleLite.attest` (dummy/stale signatures), or attester/proxy authorization missing on-chain. |
| Writer exits immediately at boot | Bad `CHAIN_WRITER_SIGNER_SURI` (see DEPLOY troubleshooting) or DB unreachable. |
| Everything down after server reboot | Containers have `restart: unless-stopped` and come back with Docker. If they don't (policy predates last deploy): `cd ~/identity-backend-v2 && sudo docker compose up -d`. |
