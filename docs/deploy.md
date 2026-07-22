# Deploy

From-zero setup of a server. Day-2 operations: [runbook.md](runbook.md).

Topology: `identity-api` and `identity-chain-writer` share the identity Postgres; `username-indexer` owns a separate projection Postgres; `dim-tickets-api` and `dim-tickets-writer` share a third, dim-tickets-owned Postgres; `invite-tickets-api` and the single-instance `invite-tickets-pool` share a fourth, invite-tickets-owned Postgres (the ticket pool holds private keys — nothing shared). Caddy routes the public API between the four HTTP services and provides automatic HTTPS. Normal updates are done by CI ([workflow](../.github/workflows/deploy.yml)) — this page is only for standing up a new machine.

Secret boundaries: the JWT **signing** seed lives only in `identity-api`; `dim-tickets-api`, `invite-tickets-api`, and `turn-api` get the **public** key (or JWKS) only. Each chain-submitting worker holds only its own signing secret: the dim inviter SURI lives only in `dim-tickets-writer`, the invite inviter SURI only in `invite-tickets-pool`. `turn-api` additionally holds `TURN_SECRET` — the HMAC key shared with the TURN relay (coturn `--use-auth-secret`); relay and issuer must be rotated together, and no other service sees it. Compose uses the root `.env` for interpolation only and gives every application container an explicit environment allowlist, so no service can read another's key.

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
```

The signer (or its proxy target) must be a registered attester authority with funds; the proxy relationship must already exist on-chain. Likewise each inviter (or the primary its `…PROXY_FOR` points at) must hold `AvailableInvites` on the `Game` / `ProofOfInk` pallets and funds for fees. A non-empty `JWT_JWKS_JSON` takes precedence over `JWT_ED25519_PUBLIC_KEY`.

## 3. Run

```bash
sudo docker compose up -d --build      # first build takes several minutes; migrations run on boot
```

Configure `/etc/caddy/Caddyfile` (replace the domain):

```caddy
identity.dotspark.app {
    handle_path /docs* {
        root * /home/ubuntu/identity-backend-v2/docs/api-reference
        file_server
    }
    handle /api/v1/usernames/search {
        reverse_proxy 127.0.0.1:8081
    }
    handle /api/v1/dim-ticket* {
        reverse_proxy 127.0.0.1:8082
    }
    handle /api/v1/invitation-ticket/* {
        reverse_proxy 127.0.0.1:8083
    }
    handle /api/v1/turn/* {
        reverse_proxy 127.0.0.1:8084
    }
    handle {
        reverse_proxy 127.0.0.1:8080
    }
}
```

```bash
sudo chmod o+x /home/ubuntu
sudo caddy validate --config /etc/caddy/Caddyfile && sudo systemctl reload caddy
```

## 4. Verify

```bash
curl -fsS http://127.0.0.1:8080/readyz          # {"chain":"up","db":"up",...}
curl -fsS http://127.0.0.1:8081/readyz          # projection freshness + dependencies
curl -fsS http://127.0.0.1:8082/readyz          # dim-tickets: {"chain":"up","db":"up",...}
curl -fsS http://127.0.0.1:8084/readyz          # turn: {"service":"turn","status":"ready"} (stateless)
curl -fsS https://<domain>/api/v1/attester
curl -fsS 'https://<domain>/api/v1/usernames/search?prefix=alice'
curl -sS https://<domain>/api/v1/dim-ticket/x   # 401 problem+json = routed + auth gate live
sudo docker compose logs identity-chain-writer | tail -5   # "connected signer=…", "acquired writer lease"
sudo docker compose logs dim-tickets-writer | tail -5      # "connected signer=…", "acquired writer lease"
```

For CI deploys to reach this server, set the repo secrets `IBV2_SSH_KEY` (private key authorized for `ubuntu@server`) and `IBV2_HOST` (server IP), and add a read-only GitHub deploy key on the server.

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
