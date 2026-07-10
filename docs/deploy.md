# Deploy

From-zero setup of a server. Day-2 operations: [runbook.md](runbook.md).

Topology: three containers (Postgres, `identity-api`, `identity-chain-writer`) behind Caddy with automatic HTTPS. Normal updates are done by CI ([workflow](../.github/workflows/deploy.yml)) — this page is only for standing up a new machine.

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
```

The signer (or its proxy target) must be a registered attester authority with funds; the proxy relationship must already exist on-chain.

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
curl -fsS https://<domain>/api/v1/attester
sudo docker compose logs identity-chain-writer | tail -5   # "connected signer=…", "acquired writer lease"
```

For CI deploys to reach this server, set the repo secrets `IBV2_SSH_KEY` (private key authorized for `ubuntu@server`) and `IBV2_HOST` (server IP), and add a read-only GitHub deploy key on the server.

## Troubleshooting

| You see | Fix |
|---|---|
| `migration N was previously applied but has been modified` | Volume holds an older schema. `sudo docker compose down && sudo docker volume rm identity-backend-v2_pgdata && sudo docker compose up -d` — destroys DB state, back up first ([runbook](runbook.md#database-backup-manual)). |
| writer: `Cannot parse hex string` | Malformed `CHAIN_WRITER_SIGNER_SURI` (see accepted formats above). |
| `identity-api` never healthy | It blocks on the People Chain RPC at startup — check connectivity to `PEOPLE_RPC_URL`. |
| No TLS certificate | DNS not propagated or 80/443 blocked. `dig +short <domain>`, `journalctl -u caddy`. |
| `/docs` returns 403/404 | Caddy can't traverse `/home/ubuntu` — re-run the `chmod o+x` line. |
