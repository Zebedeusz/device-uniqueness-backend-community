# Deploying Identity Backend V2

Rust identity service (auth handshake + username registration on People Chain), deployed as three Docker Compose containers (Postgres, `identity-api`, `identity-chain-writer`) behind a Caddy reverse proxy with automatic HTTPS.

For ongoing operation (logs, redeploys, outbox inspection) see [RUNBOOK.md](RUNBOOK.md).

## Prerequisites

- Ubuntu 24.04 server with sudo and outbound internet (the api/writer connect to a public People Chain RPC at startup)   (check: `lsb_release -rs`)
- A DNS **A record** for your domain pointing at the server's public IP (direct, not proxied)   (check: `dig +short <your-domain>` returns the server IP)
- Ports 80 and 443 reachable from the internet (Caddy), port 22 for SSH
- Access to the People Chain attester signing key (see step 3) — ask the IB team

## 1. Install Docker and Caddy (on the server)

```bash
sudo apt-get update
sudo apt-get install -y docker.io docker-compose-v2 ca-certificates curl gnupg
sudo systemctl enable --now docker

curl -1sLf "https://dl.cloudsmith.io/public/caddy/stable/gpg.key" | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf "https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt" | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt-get update
sudo apt-get install -y caddy
```

## 2. Get the code

```bash
git clone git@github.com:paritytech/identity-backend-rust.git ~/identity-backend-v2
cd ~/identity-backend-v2
```

No local Rust toolchain is needed — the binaries build inside Docker.

## 3. Configure

Create `~/identity-backend-v2/.env` (Compose auto-loads it; `chmod 600 .env`):

```bash
BIND_ADDR=0.0.0.0:8080
PEOPLE_RPC_URL=wss://paseo-people-next-system-rpc.polkadot.io
# 32-byte Ed25519 seed for JWT signing (generate: openssl rand -hex 32, prefix with 0x)
JWT_ED25519_SECRET=0x<64-hex>
# Public key of the attester authority served by GET /api/v1/attester
ATTESTER_PUBLIC_KEY=0x<64-hex attester public key>
AUTH_ENABLED=false
ENFORCE_AUTH=false
RUST_LOG=info
# Writer signing key. Accepts a mnemonic, a 0x+64-hex 32-byte seed, or a
# 0x+128-hex raw 64-byte sr25519 secret (polkadot.js format).
CHAIN_WRITER_SIGNER_SURI=<secret — never commit>
# SS58 of the cold attester primary the hot key proxies for (empty = sign directly)
CHAIN_WRITER_PROXY_FOR=<attester SS58 or empty>
```

Notes:
- The signer account (or its proxy target) must be a registered attester authority on the target People Chain, with funds for fees. The proxy relationship must already exist on-chain.
- `AUTH_ENABLED=false` is the current test mode: platform attestation is a no-op, JWTs are still issued and account proofs verified.

## 4. Deploy

```bash
cd ~/identity-backend-v2
sudo docker compose up -d --build
```

First build compiles the Rust workspace inside Docker (several minutes). Database migrations run automatically on boot.

Configure Caddy (`/etc/caddy/Caddyfile`), replacing the domain:

```bash
sudo tee /etc/caddy/Caddyfile >/dev/null <<'CADDY'
identity.dotspark.app {
    handle_path /docs* {
        root * /home/ubuntu/identity-backend-v2/docs/api-reference
        file_server
    }
    handle {
        reverse_proxy 127.0.0.1:8080
    }
}
CADDY
sudo chmod o+x /home/ubuntu
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

Caddy obtains and renews the Let's Encrypt certificate automatically.

## 5. Verify

```bash
curl -fsS http://127.0.0.1:8080/readyz
# → {"chain":"up","db":"up","service":"identity-service","status":"ready"}

curl -fsS https://<your-domain>/readyz
curl -fsS https://<your-domain>/api/v1/attester
# → {"attester":"0x<ATTESTER_PUBLIC_KEY>"}

curl -fsS -X POST https://<your-domain>/api/v1/auth/challenges
# → {"challenge":"<base64>"}
```

- API reference: `https://<your-domain>/docs`
- Writer log should show `identity-chain-writer connected signer=0x…` and `acquired writer lease`:
  `sudo docker compose logs identity-chain-writer | tail -5`

## Troubleshooting

| You see | Fix |
|---|---|
| `migration 2 was previously applied but has been modified` | The Postgres volume holds an older schema. Use a fresh volume: `sudo docker compose down && sudo docker volume rm identity-backend-v2_pgdata && sudo docker compose up -d` (destroys DB state — take a manual backup first, see RUNBOOK.md "Database backup"). |
| writer: `Cannot parse hex string: Invalid string length` | `CHAIN_WRITER_SIGNER_SURI` is malformed. Use a mnemonic, `0x`+64-hex seed, or `0x`+128-hex raw sr25519 secret. |
| writer: `Cannot encode transaction extension 'RestrictOrigins'` or a wasm trap in `TaggedTransactionQueue_validate_transaction` | Running a pre-fix build. Pull latest `main` and rebuild (`sudo docker compose up -d --build`). |
| `identity-api` never becomes healthy | It blocks on People Chain RPC at startup. Check outbound connectivity to `PEOPLE_RPC_URL` and the container log. |
| Caddy serves no certificate | DNS not propagated to this server yet, or port 80/443 blocked. Check `dig +short <domain>` and `journalctl -u caddy`. |
| `/docs` returns 403/404 | Caddy (user `caddy`) can't traverse `/home/ubuntu`. Re-run the `chmod o+x /home/ubuntu` line from step 4. |
