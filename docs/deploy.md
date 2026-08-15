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
DOTNS_GATEWAY_ENABLED=false              # REQUIRED here, see below: the compose
                                         # default is `true` and pairs with
                                         # PreviewNet, not with the Paseo People
                                         # endpoint on the line above
JWT_ED25519_SECRET=0x<64-hex>            # openssl rand -hex 32
ATTESTER_ACCOUNT=<SS58>                  # attester authority: published by GET
                                         # /api/v1/attester AND attested as by
                                         # the writer (one value, cannot drift)
AUTH_ENABLED=false                       # test mode: attestation is a no-op
ENFORCE_AUTH=false
RUST_LOG=info
CHAIN_WRITER_SIGNER_SURI=<secret>        # mnemonic, 0x+64-hex seed, or 0x+128-hex raw sr25519
                                         # its account == ATTESTER_ACCOUNT -> direct signing,
                                         # otherwise the writer proxies for the authority
# dim-tickets (see .env.example for tunables and dev defaults)
DIM_TICKETS_POSTGRES_PASSWORD=<URL-safe random password>
PEOPLE_NETWORK=<westend2|paseo|polkadot>
INVITER_ADDRESS=<dim inviter SS58>        # its on-chain AvailableInvites funds dim tickets
JWT_ED25519_PUBLIC_KEY=0x<64-hex>         # PUBLIC half of JWT_ED25519_SECRET (or set JWT_JWKS_JSON)
INVITER_SIGNER_SURI=<secret>              # same accepted formats as the chain writer
                                          # its account == INVITER_ADDRESS -> direct signing,
                                          # otherwise it proxies for the inviter
# invite-tickets (distinct account from the dim inviter — see Nonce lanes above)
INVITE_INVITER_ADDRESS=<invite inviter SS58>   # funds set_invite_ticket; stamped into ticket rows
INVITE_INVITER_SIGNER_SURI=<secret>            # hot invite inviter key, or a delegate proxying for it

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

**`DOTNS_GATEWAY_ENABLED=false` is load-bearing in the block above, not decoration.**
The compose defaults are a matched PreviewNet pair — `PEOPLE_RPC_URL` at PreviewNet's
People chain and `ASSET_HUB_RPC_URL` at PreviewNet's Asset Hub — with the lane on.
This environment overrides only the first of those two, so leaving the flag out would
run the writer against Paseo's People chain while claiming `LiteLabelOwner` on
PreviewNet's Asset Hub. `reserve_name` writes a **global** claim on the label, and
**it cannot be undone** — only the client holds the key that could re-sign. Omission
used to mean off; it now means on. Any environment that repoints `PEOPLE_RPC_URL`
away from PreviewNet without repointing `ASSET_HUB_RPC_URL` to the same network must
set this flag to `false` in the same edit.

Each primary must already hold its on-chain privilege: `ATTESTER_ACCOUNT` a registered attester authority with an attestation allowance, and each inviter (`INVITER_ADDRESS` / `INVITE_INVITER_ADDRESS`) `AvailableInvites` on the `Game` / `ProofOfInk` pallets. Each signing key must hold funds for fees. Proxying is never configured: whenever a signing key's own account differs from the primary it is named against, the call is wrapped in `Proxy.proxy(real = primary)` — so that delegation must already exist on-chain. A non-empty `JWT_JWKS_JSON` takes precedence over `JWT_ED25519_PUBLIC_KEY`.

The `notify-relay` provider secrets are optional: with none set the relay still boots and reports per-request provider failure for the unconfigured platform. `JWT_ED25519_PUBLIC_KEY` must be the public half of `identity-api`'s `JWT_ED25519_SECRET`.

The `turn-api` variables have compose dev defaults (placeholder secret, `example.org` realm, `example.com` ICE URLs), so leaving them unset boots a service that mints credentials **no real relay accepts**. `TURN_SECRET` is the base64 encoding of the relay's raw `static-auth-secret` string — base64-encoding a hex-decoded form produces different HMAC keys and the relay answers 401 to every Allocate.

## 3. Run

```bash
# the ten service images — one cargo build, ten thin runtimes (several minutes
# the first time). `bake` is the only builder: compose's per-service `build.target`
# keys exist so `docker compose build` works too, but bake shares the compile.
sudo docker buildx bake all

# the environment (migrations run on boot, advisory-locked)
sudo docker compose up -d --no-build

# the public edge — acquires its certificate on first start
sudo docker compose -f gateway/docker-compose.yml -p edge up -d

# monitoring — its own project, so an environment restart never stops it
sudo docker compose -f observability/docker-compose.yml -p observability up -d
```

Every process serves Prometheus metrics on port 9090 (`METRICS_ADDR`), published to no
host port and routed by no Caddy rule: they are reachable only from the `ibv2-metrics`
network, under the same `<service>-$ENV_ID` alias Prometheus scrapes in
[`observability/prometheus.yml`](../observability/prometheus.yml), which holds one job per
environment labelled `env_id`. Adding an environment means copying that job with the other
`ENV_ID` suffix, listing **only the services that environment deploys** (an undeployed
target is a permanent `down`), and keeping the `env_id` label — it is what the dashboard
filters on, so a job without it merges two environments into one line. The Prometheus UI is
loopback-only (`127.0.0.1:9091`) — reach it over an SSH tunnel, and set
`METRICS_ENABLED=false` to turn an environment's exporters off.

The same project also runs the logs half: **Loki** stores them (14-day retention,
[`observability/loki-config.yaml`](../observability/loki-config.yaml)), **Alloy** ships them
by discovering this host's containers through a read-only Docker socket
([`observability/alloy-config.alloy`](../observability/alloy-config.alloy)), and **Grafana**
is the single UI over both datasources, loopback-only on `127.0.0.1:3000`
(`GRAFANA_PORT`) with anonymous read-only access and the committed
`Identity Backend V2 — overview` dashboard. Nothing has to be configured per service:
`LOG_FORMAT=json` (compose default) is all a process contributes, and the labels
(`project`, `service`, `container`, `level`) come from Docker metadata. Set
`GRAFANA_ADMIN_PASSWORD` in the observability project's environment before exposing that
port anywhere but loopback.

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

## Choosing a topology

This backend deploys in one of two shapes. They serve an **identical** public API; they differ in
how many processes hold how many secrets. The design reasoning and the full threat model are in
[`architecture.md`](architecture.md#deployment-topologies) — read that before choosing, not after.

| | standard | small |
|---|---|---|
| workloads | 10 | 5 |
| HTTP tier | six services, one per surface | one `all-in-one` process |
| workers | four singletons | the same four |
| `JWT_ED25519_SECRET` reaches | `identity-api` only | the process that also serves public search |
| `/readyz` on a dead dependency | that service leaves rotation | reports `degraded`, stays in rotation |
| values file | `values-<env>.yaml` | see `helm/values-small.yaml` |

**The standard topology is the default and is what `paseo-next-v2` and `previewnet` run.** Moving
either of them to the small topology is a security decision, not a configuration change.

Pick the small topology only when operating ten workloads is genuinely not worth it, and only with
the trade understood: one process holds `JWT_ED25519_SECRET` (mint a token for any subject),
`POC_HMAC_SECRET`, `TURN_SECRET` and all four database URLs, in the same address space that serves
the unauthenticated public `GET /api/v1/usernames/search`.

The chart will not let you reach it by accident:

- the six per-service workloads and `all-in-one` are **mutually exclusive** — a mix fails to render;
- `global.allInOne.acknowledgedSecretUnion: true` is **required**, and absent by default, so
  copying another environment's values file does not get you here;
- a per-service `secrets:` entry left behind fails to render — a credential nobody deploys is a
  credential nobody rotates.

### Moving between topologies

It is a **full redeploy, not a scale operation**: different workloads, different Secrets, a
different ingress target. Migrate by applying the other values file — the databases, the chain
accounts and the vault entries are unchanged, since both shapes run the same code against the same
four databases. Expect the API to be briefly unavailable while the workloads swap, and note that the
small topology's first boot carries the username-indexer's finalized scan (`startupSeconds: 350`).

There is no in-place path and there should not be one: the two shapes have different secret
distributions, and a "gradual" migration would mean running both at once, which is exactly the mix
the chart refuses.

## Adding an environment

A second environment is another compose project on the same host with its own
`.env`, plus a DNS record and a site block. It shares the machine, the image,
and the edge; it shares no database, no signing key, and no chain.

1. **Check the chain is usable.** For PreviewNet: the environment's
   `ATTESTER_ACCOUNT` authority `5GCF223UbXNZiH78V6DhsWeZ6hrQ6xRfGTfdn6MraprJKbWj`
   must still list our writer signer as an `Any`/delay-0 proxy, hold an attestation
   allowance, and hold invites — see the post-wipe sequence in
   `docs/plans/active/previewnet-env.md`.
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
   ATTESTER_ACCOUNT=5GCF223UbXNZiH78V6DhsWeZ6hrQ6xRfGTfdn6MraprJKbWj
   QUEUE_ENABLED=false
   ```

   **The dotNS gateway lane needs nothing here.** `DOTNS_GATEWAY_ENABLED` defaults
   to `true` and `ASSET_HUB_RPC_URL` defaults to
   `wss://previewnet.substrate.dev/asset-hub`, which is this environment's own Asset
   Hub. PreviewNet is the network the defaults are cut for, so it inherits them.
   Setting them explicitly does no harm and documents the intent:

   ```bash
   DOTNS_GATEWAY_ENABLED=true                                    # compose default
   ASSET_HUB_RPC_URL=wss://previewnet.substrate.dev/asset-hub    # compose default
   ```

   On, identity-api accepts the `dotns` block AND identity-chain-writer submits
   `DotnsGateway::reserve_name` on Asset Hub. Both halves read the same variable;
   a split value is intake accepting blocks nothing ever writes.

   **The two URLs must name the same network.** `reserve_name` writes
   `LiteLabelOwner`, a global claim on the label, so a writer attesting usernames
   on one network's People while claiming their labels on another's Asset Hub
   produces a claim on the wrong chain that **cannot be undone** — only the client
   holds the key that could re-sign. The defaults are a matched PreviewNet pair, so
   the mismatch can only come from overriding one without the other. That is now
   `paseo-next-v2`'s job rather than PreviewNet's: it overrides `PEOPLE_RPC_URL` to
   Paseo next, so it **must** carry `DOTNS_GATEWAY_ENABLED=false`, because Paseo
   next's Asset Hub still runs the older `reserve_name` (`ring_vrf_key` +
   `proof_of_ownership`, no `signed_at`). Should it ever be enabled there against
   Paseo's own Asset Hub, the writer's shape assertion catches it and parks the
   dotNS lane with both argument lists named — People registration keeps running.
   Against PreviewNet's Asset Hub the shape assertion **passes** and the wrong-chain
   claim goes through, which is why the flag, not the assertion, is the guard.
   Enable it there once that runtime is upgraded.

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

## Kubernetes (Helm)

**Status: not deployed anywhere yet.** The chart renders and is gated in CI; no cluster has
been provisioned. Provisioning is SRE's — see [`sre-deployment-request.md`](sre-deployment-request.md)
— and the sequencing lives in [`plans/active/kubernetes-helm-deploy.md`](plans/active/kubernetes-helm-deploy.md).
Everything above (the single host, compose, the Caddy edge) is what actually serves traffic today.

The chart is [`helm/`](../helm): one loop over ten workloads, plus a static server for the API
reference. One values file per environment.

```bash
helm template ibv2 helm -f helm/values-paseo-next-v2.yaml --set global.imageTag=<sha>
```

It refuses to render rather than deploy something nobody chose. It will fail if: `imageTag` or
the ingress host is unset; a single-instance worker has `replicas != 1`; the two inviters share a
signing account; a secret is missing, plaintext, or handed to a workload that does not declare it;
`JWT_ED25519_PUBLIC_KEY` or `ATTESTER_ACCOUNT` is still the committed dev value; the dotNS lane is
on without an Asset Hub; the queue is on without its advancer; or metrics are off while the
workers are probed on `:9090`.

Compose and the chart are **two renderings of one config contract**, and CI keeps them in step:

| Gate | What it holds | Where |
|---|---|---|
| `verify_compose_boundaries.sh` | no service reads another's secret (compose) | offline |
| `verify_role_split.sh` | one image, one tag; every compose service and chart workload names a role `ibv2 --list-roles` accepts, exactly once, and its name IS its role; no default role in the image; release exporter shape | offline (+ `RUNTIME_CHECK=1` against the published image) |
| `verify_helm_render.sh` | same, on the rendered chart; env-key parity with compose; probe budgets cover compose's | offline |
| `verify_route_parity_live.sh` | real Caddy and real Traefik answer every route class identically | its own job (needs docker) |

Run the offline three with `just verify-config`, the last with `just verify-routes`.

Differences from the compose topology, and why:

- **Migrations are unchanged** — each service applies its own schema on boot under an advisory
  lock. There is deliberately no migration Job: one would let a workload migrate a database it
  otherwise never touches.
- **`/docs`** is a ConfigMap behind a static server, with a `stripPrefix` middleware standing in
  for Caddy's `handle_path`.
- **turn-api and notify-relay are shared**: an environment that does not deploy them still routes
  to the instances that do, via `global.sharedServiceNamespace`.
- **The APNs key** is a Secret volume at mode `0440` with `fsGroup: 10001`, not a host path.

## Cutting a release

Open a PR whose **title is `chore(release): vX.Y.Z`**, containing the version bump and the
changelog entry — `[workspace.package] version` in `Cargo.toml`, and a `## [X.Y.Z]` section in
`CHANGELOG.md`. Merging it does the rest:

```
merge "chore(release): v0.2.0"
  ├─ release-on-merge.yml   validates title == Cargo.toml == CHANGELOG, that the commit
  │                         is on main, and that the tag is new, THEN tags the merge commit
  │                         └─ calls release.yml: the check suite, ten images to ghcr +
  │                            Docker Hub, binaries, compose bundle, SHA256SUMS
  └─ deploy.yml             the ordinary `push: main` lane — builds SHA-tagged images
                            and rolls the host, exactly as any other merge
```

Everything that can be checked without the tag is checked **before** it is created — the title
shape, title/`Cargo.toml`/`CHANGELOG.md` agreement, that the commit really is on `main`, and that
the tag does not already exist. Those are the mistakes a release PR makes, and a tag pointing at a
release that never happened is a version nobody can re-cut without deleting it first.

The release proper runs *after*, because `release.yml` is cut from the tag. If it fails — a flaky
suite, a missing registry credential — the tag is published and the release is not: delete the
tag, fix, and merge a new release PR.

Two mechanics worth knowing:

- **The tag is created, then `release.yml` is *called*, not triggered.** A tag pushed with
  `GITHUB_TOKEN` does not fire `on: push` — GitHub blocks a workflow from starting another with
  the default token. Pushing and hoping would create the tag and release nothing.
- **Manual tagging still works**: `git tag v0.2.0 && git push --tags` runs the same workflow.
  The merge lane is a convenience, not a replacement.

A pre-release version (`v0.2.0-rc1`) is detected from the `-` and marks the GitHub Release as a
pre-release, and leaves `stable` pointing at the last stable images.

## Release images

Pushing a `v*` tag runs [`release.yml`](../.github/workflows/release.yml), publishing a
**ten** multi-arch (`linux/amd64` + `linux/arm64`) images —
`ghcr.io/paritytech/identity-backend-rust/<service>` — each tagged `1.2.3`, `v1.2.3` and
(for a non-pre-release) `stable`. Each architecture builds on its own native runner, pushing
by digest; the digests are then joined into one manifest list per service, so a single tag
serves both architectures and all ten services move together. Releases are independent of the `main` deploy above: tagging never
restarts production, and `latest` keeps tracking `main`. Tag a commit that already passed
CI on `main` — the release workflow builds and publishes without re-running the test gate.

```bash
git tag v0.1.0 && git push origin v0.1.0
```

The same digests are published to two registries:

| Registry | Tags | Who it is for |
|---|---|---|
| `ghcr.io/paritytech/identity-backend-rust/<service>` | `vX.Y.Z`, `X.Y.Z`, `stable`, `latest` | our own deploys — inherits the repo's **private** visibility |
| `docker.io/paritytech/ibv2-<service>` | `vX.Y.Z`, `X.Y.Z`, `stable` | **public**, unauthenticated `docker pull` for external consumers |

Docker Hub namespaces are one level (`org/name`), so the mirror flattens the ghcr path into an
`ibv2-` prefix: **ten repositories**, `paritytech/ibv2-identity-api` … `paritytech/ibv2-notify-relay`,
not one. Override the prefix with the `DOCKERHUB_PREFIX` repository variable; the credentials are
the `DOCKERHUB_USER` / `DOCKERHUB_PASSWORD` secrets.

Each of the ten must exist **and be public**, and `DOCKERHUB_PASSWORD` must be a token with
**write** scope on that namespace. A push can auto-create a repository, but only where the
account may create one — and the org's default visibility decides what it is created as, so a
push that does succeed may well produce ten *private* repos, defeating the entire point of the
mirror. `latest` is deliberately ghcr-only: it tracks `main`, and unreleased work should not be
what an outside consumer picks up by default.

**The mirror is best-effort; ghcr is not.** ghcr is the lane our own deploys pull from, and it
must never be held hostage by a Docker Hub token — so the two registries are pushed by separate
steps, ghcr first and alone. Every Docker Hub failure is a `::warning::` rather than a release
failure: `DOCKERHUB_USER` unset, a token the registry rejects, or a repository that a
credential-free pull cannot reach (checked at release time, asked with no credentials at all, so
a private repo is caught here rather than on someone else's laptop). In each case the release
notes and the compose bundle fall back to ghcr pull lines, because the notes must never point at
a registry the reader cannot reach. A release is therefore *never* blocked on Docker Hub — but
check the run's warnings if `docker.io` tags do not appear.

The ghcr packages inherit the repo's private visibility, so external consumers need a GitHub
token with `read:packages` (SSO-authorized for `paritytech`) to pull. Make the package
public only as a deliberate decision — it publishes binaries of a private repo.

To run the released image rather than build it, set `IMAGE_REPO` and `IMAGE_TAG` and pass
`--no-build` (`IMAGE_REPO=ghcr.io/paritytech/identity-backend-rust IMAGE_TAG=v0.2.0
docker compose pull`). **Every service resolves to the same `$IMAGE_REPO:$IMAGE_TAG`** and
becomes itself through its `command: ["--role", "<service>"]`; `ibv2 --list-roles` prints the
set. A per-service image or tag pin makes "which build is this environment on?" unanswerable,
and `scripts/verify_role_split.sh` fails the build if the compose file drifts that way. The
roles each fail fast on missing configuration:

| Service | Required |
|---|---|
| `identity-api` | `DATABASE_URL`, `PEOPLE_RPC_URL`, `JWT_ED25519_SECRET`, `ATTESTER_ACCOUNT` |
| `identity-chain-writer` | `DATABASE_URL`, `PEOPLE_RPC_URL`, `CHAIN_WRITER_SIGNER_SURI`, `ATTESTER_ACCOUNT`, and `ASSET_HUB_RPC_URL` wherever `DOTNS_GATEWAY_ENABLED` is on — which is the compose default, so it is required unless the environment turns the lane off |
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
