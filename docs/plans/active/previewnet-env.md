> Stage: active. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals

- A second live deployment of this backend bound to **PreviewNet** (`wss://previewnet.substrate.dev/people`, para 1502), on its own hostname alongside the existing Paseo Next v2 deployment at `https://identity.dotspark.app`.
- Hosts that switch to PreviewNet (Brevity desktop has the switch, gated off today) get a working device-attestation backend on that network: auth handshake, username registration on PreviewNet People, username search.
- Both deployments run on the **existing** `dub` machine and ship from the same `main` push.
- One binary set works against both People runtimes — no per-network fork of `chain-types`, no second image.
- The public edge becomes a committed, reproducible artifact instead of host state.

# Non-goals

- **No new server.** The whole current stack uses ~1 GB RAM and 9 GB disk on a 4 vCPU / 8 GB / 90 GB host at load 0.45; a second stack fits.
- **No header- or path-based selection.** An environment is a hostname: a wrong or missing value must fail, not resolve to the other chain (decided 2026-08-03, reversing an earlier header design — see Approved architecture).
- **No multi-network process.** Each service keeps one chain connection; two isolated compose projects, each single-network.
- **No DevNet yet.** `products-devnet` (public community Paseo, `wss://people-paseo.gatotech.network`, specName `people-paseo` 2004002) is a third lineage that additionally needs subxt's real `CheckMetadataHash` extension, and its attester authority is controlled by PCF rather than by us. Deliberately out of scope until these two envs are running.
- **No new capability.** No push/notify work, no attestation changes. The legacy-origin cutover lane is *removed* rather than extended: nothing was ever configured behind it, so those routes 404 exactly as they did before. No `dim-tickets`, no `registration-queue`, no DotNS intake on the new env.
- No production-grade key custody: both are test deployments.

# Design principles

- User journey drives order: signing on PreviewNet must work before an environment is worth exposing.
- Reuse before rebuild: the second env is the **same** `docker-compose.yml`, the **same** `gateway/Caddyfile`, and the **same** image — parameterized, never copied.
- Risky migrations happen alone: the edge moves into a container while there is still only one environment, so a failure has one cause.
- Fail loud, never guess: a request that does not name an environment must not reach one. With a hostname there is no default to fall through to.
- Isolation by default: separate compose project, volumes, databases, `.env`, JWT signing key, and chain accounts per env.

# User journey

1. A host (Brevity desktop, Nova) switches its network to PreviewNet.
2. It points at that environment's backend URL — the per-environment field its config already carries.
3. It completes the auth handshake and receives a JWT signed by that environment's key.
4. It registers a username; the extrinsic lands on PreviewNet People and is visible on-chain.
5. Username search on that environment returns the newly registered name.
6. Clients configured for Paseo Next v2 keep using `https://identity.dotspark.app`, untouched.

# Approved architecture (decided — do not relitigate while building)

- **Selection**: one hostname per environment — `identity.dotspark.app` (Paseo Next v2) and `identity-previewnet.dotspark.app` (PreviewNet). A second site block in `gateway/Caddyfile` reuses the same `handle` list against the `*-previewnet` aliases; the edge container already owns 80/443, so the certificate is automatic. Chosen over a header (an earlier decision, reversed 2026-08-03) because:
  - a missing header silently means Paseo, so a forgotten header registers a username on the **wrong chain**; a wrong hostname simply fails;
  - clients already store a **per-environment** backend URL (Brevity's `HostConfig.preview.identity.backendUrl`), so a URL costs them a data edit while a header costs code;
  - one origin for two JWT issuers breaks anything caching JWKS by URL, and collapses logs/Sentry for both environments into one host.
- **Edge**: the gateway runs **in a container** (its own small compose project) owning 80/443 and a certs volume, joined to a shared external docker network. **No application or Postgres container publishes a host port.**
- **Per-env services**: `device-attestation-api`, `device-attestation-chain-writer`, `username-indexer`, and their two Postgres instances. **`invite-tickets` is not deployed to PreviewNet** (decided 2026-08-03): the Game/ProofOfInk invites live on the shared authority `5F7H1LkZ…`, whose only registered delegate is the account the chain-writer already signs as — two submitters on one account and one chain is the nonce race the contracts forbid. It also loses no parity, because the Paseo pool is itself non-functional (running the compose `//Bob` default, unfunded, zero tickets). Revisit by asking the operator for a second delegate.
- **Shared services** (one instance for all envs): `turn-api` and `notify-relay`. They hold no chain connection and no per-chain state; they verify device-attestation JWTs from a **merged JWKS** — tokens carry a `kid` derived from the signing key (`crates/device-attestation/src/jwt.rs:61`) and `Verifier::from_jwks` selects among many keys (`crates/jwt-verify/src/lib.rs:99-165`), so accepting both issuers is configuration, not code.
- **Cross-runtime signing**: one transaction-extension tuple carrying both slot-0 names. Verified in subxt 0.50: extensions are encoded by walking the **live** metadata list and resolving tuple members **by name**, so a member the runtime does not declare is never encoded and cannot shift positions.
- **Host layout**: **one checkout per environment** (`~/dub-paseo-next-v2`, `~/dub-previewnet`), each holding exactly one `.env`; the CI loop names both. The alternative — a single checkout with `.env.paseo-next-v2` / `.env.previewnet` selected by `--env-file` — was considered and rejected (decided 2026-08-03): compose auto-loads `./.env` whenever `--env-file` is absent, so one forgotten flag runs the PreviewNet project (its own volumes, its own database) against Paseo's RPC and Paseo's signer with no error at all. That is the same silent-default failure the hostname decision above exists to prevent, and a directory per environment makes it structurally impossible rather than a habit. Accepted costs: the edge always deploys from the primary checkout, and a third environment means another clone — revisit then, and note that a single checkout only becomes safe if no plain `.env` exists in the tree.
- **Network label**: `PEOPLE_NETWORK` stays `paseo` wherever it is read. It is a frozen legacy wire literal that shipping clients parse (`crates/invite-tickets/src/tickets.rs`, DB `CHECK` in `crates/invite-tickets/migrations/0001_invite_tickets.sql:15`), so emitting an unknown value risks a client-side decode failure. Only the ticket services read it, and they are not deployed to PreviewNet, so the question is currently moot there — it returns if `invite-tickets` ever ships on another environment. Not to be confused with `ENV_ID`, which **does** follow the ecosystem ids (`paseo-next-v2`, `previewnet`).

# Foundations / Already shipped (✅)

## Single-host multi-service stack

- What exists: `docker-compose.yml` — the service set, per-service env allowlists, and secret boundaries enforced by `scripts/verify_compose_boundaries.sh`.
- ✅ Demo: `sudo docker compose ps` on `dub` shows the stack healthy.
- Gaps: every app and Postgres service publishes a host port (`8080`–`8085`, `5432`, `5433`, `5435`, gateway `8000`), mostly on **all interfaces**; only dim-tickets' Postgres binds `127.0.0.1`.

## Committed gateway config + a compose gateway service

- What exists: `gateway/Caddyfile` as the single route table with `{$VAR:default}` placeholders, already used both by host Caddy (prod defaults) and by a `gateway` compose service on `:8000`; CI validates before installing.
- ✅ Demo: `caddy validate --config gateway/Caddyfile`.
- Gaps: one site block and one upstream set — the second environment's hostname does not exist yet.

## CI build + deploy

- What exists: `.github/workflows/deploy.yml` — path-gated build to `ghcr.io`, SSH deploy into `~/dub-paseo-next-v2`, Caddy reload, public `/readyz` check, one concurrency group.
- ✅ Demo: a deployable push to `main` updates `identity.dotspark.app`.
- Gaps: one target directory, one health URL, no notion of a second project.

## Chain surface

- What exists: `crates/chain-types` (static subxt codegen from `metadata/people.scale`) + `chain-client` (`OnlineClient`, reconnecting RPC, `WriterSigner`). Registration is a **dynamic** call by name (`crates/device-attestation/src/chain/writer.rs:485`), so pallet-index differences between runtimes are irrelevant.
- ✅ Demo: the Paseo deployment registers usernames on-chain (verified 2026-07-09).
- Gaps: the tuple in `crates/chain-types/src/lib.rs` names slot 0 `AuthorizeValueTransfer`, which PreviewNet calls `UnitTransactionExtension`. Verified 2026-08-01: PreviewNet People is `next-people-paseo` 1000030/tx 4 vs Paseo v2's 1000029/tx 3, and **every** item this workspace uses is shape-identical (`Resources::UsernameOwnerOf`, `Consumers`, `UsernameReservationQueue`, `Game`/`ProofOfInk::AvailableInvites` + `set_invite_ticket`, `Utility::force_batch`, `Proxy::proxy`, `System::Account`, `PeopleLite::attest`, SS58 42). `transactionVersion` needs nothing: `OnlineClient` feeds the live value to `CheckTxVersion`, and `CheckMetadataHash` is not in this tuple.

# Phase 0 — external prerequisite ✅ done 2026-08-03

Not a coding phase; it is the long pole, so it must not start after Phase 1.

- [x] Request an attester authority on the freshly wiped PreviewNet — **already provisioned, nothing to request** (verified on-chain 2026-08-03). PreviewNet reuses the shared attester/inviter authority `5F7H1LkZi8rnSH8PvUsp4LD2WkvcpnjkyQyvLPu1ntbhNr5T` (the same account paseo-next, Summit, Brevity's `host_config.dart`, and `mission-control/infra/flow-attester.env.example` use), and **our** writer signer `5HEhiB3hMb8P36L6Edj716fR4wLPUSqoZZpcUrrYEod7f3qu` is already registered on it as an `Any` proxy with delay 0 — so this env ran with the shared authority `5F7H1…` and the Paseo writer secret, unchanged. **Superseded 2026-08-06:** PreviewNet now uses IB-V2's own authority `5GCF223UbXNZiH78V6DhsWeZ6hrQ6xRfGTfdn6MraprJKbWj` (allowance + game invites granted by Cosmin on 2026-08-05, `Any`/delay-0 proxies granted to writer `5HEhiB3h…` and pool `5GbVR6pw…` on 2026-08-06), configured as `ATTESTER_ACCOUNT`. On-chain state: `PeopleLite::AttestationAllowance[5F7H1…]` = 999851, `Game::AvailableInvites[5F7H1…]` = `ProofOfInk::AvailableInvites[5F7H1…]` = 1000000, writer signer funded (~99.99 units, nonce 142). 149 accounts are already attested post-wipe (`PeopleLite::LitePeople` = `Resources::Consumers` = `Resources::UsernameOwnerOf` = 149, e.g. `loadtestvua.52`).
- [x] Record who owns and can re-fund it: the authority is not ours — it is the shared environment attester held by the network operators (SRE / legacy identity-backend), so re-provisioning after a wipe is an ask, not something we can do. PreviewNet is volunteer-run; **Utkarsh** is the access/spawn contact (per Applied Engineering Squad, 2026-07-31), and `paritytech/previewnet-v2` holds the ansible runbook for the box. PreviewNet people sudo is `5DFsPMSY4jgZf9m8NKoZAhv5C2xcVSepTRYitaTwMULt8R5Y` (relay: `12CAXghbvWx36gmeKxrZJrkE3exGBkCxXvHD3sTHuZNQJmU4`) — an operator key, not a well-known dev key.
- [x] Repeatable post-wipe sequence. Verify first, ask only if a check fails (all queries via `dot`, chain alias `previewnet-people`):
  1. `dot previewnet-people.query.Proxy.Proxies 5GCF223UbXNZiH78V6DhsWeZ6hrQ6xRfGTfdn6MraprJKbWj` — expect our writer signer as an `Any`/delay-0 delegate. Missing → ask for `Proxy.add_proxy(delegate = 5HEhiB…, Any, 0)` from the authority.
  2. `dot previewnet-people.query.PeopleLite.AttestationAllowance --dump` — expect a non-trivial allowance for `5GCF223U…`. Missing → ask for `PeopleLite.increase_attestation_allowance(5GCF223U…, n)` (root origin).
  3. `dot previewnet-people.query.Game.AvailableInvites --dump` and `…ProofOfInk.AvailableInvites --dump` — expect invites for `5GCF223U…`. Missing → ask the same owner to top them up.
  4. `dot previewnet-people.query.System.Account 5HEhiB3hMb8P36L6Edj716fR4wLPUSqoZZpcUrrYEod7f3qu` (and the authority) — expect a funded free balance for fees.
  5. Reset the environment's own state, which the wipe invalidated too: `scripts/reset_env_state.sh --project dub-previewnet --confirm dub-previewnet` (`--dry-run` first to see the counts). It clears the registration outbox and payment quotes and leaves the device keys and minted vouchers alone — the rationale per table is in the script header and `docs/runbook.md` → **After a chain wipe**. Skipping it leaves ASSIGNED rows for usernames the new chain has never heard of, and every one of them keeps occupying a discriminator: availability unions the whole outbox with no status filter, so bases go `EXHAUSTED` against an empty chain. The username-indexer needs nothing here — since `sync_state.genesis_hash` it detects the new chain at boot and rebuilds its own projection.
  6. Only then restart the PreviewNet stack; the writer logs `connected signer=…` and the first extrinsic should finalize.
- **✅ Demo** (passed 2026-08-03): attester authority, its proxy delegation to our writer signer, attestation allowance, and both invite pools are all visible on PreviewNet People on-chain, with the writer signer funded.

# MVP phases (ship-shaped, demoable)

## Phase 1: one binary, both runtimes ✅ done 2026-08-03

- **Goal**: the existing binaries read and submit against PreviewNet People with no second codegen and no second image.
- **Scope checklist**:
  - [x] Add `Noop<UnitTransactionExtension>` to `PeopleTransactionExtensions` and the matching `()` in `PeopleExtrinsicParamsBuilder::build` (`crates/chain-types/src/lib.rs`). PreviewNet's value type is `()` so it encodes nothing; Paseo v2's stays `Option<[u8; 64]>` = `None`.
  - [x] Test that the tuple resolves against **both** runtimes' extension name lists (`KNOWN_RUNTIMES` + `TUPLE_EXTENSIONS` in the `chain-types` tests).
  - [x] Left `metadata/people.scale` untouched — no used item demanded a refresh.
  - [x] `just check` green.
- **✅ Demo (passed 2026-08-03)**: one throwaway binary built against `chain-types` + `chain-client` signed a `System::remark` as the funded dev account and drove it to **finalized on both chains** — PreviewNet (`wss://previewnet.substrate.dev/people`, block `0xc537ad6b…`) and Paseo Next v2 (block `0x6cf28e76…`). A mis-encoded extension payload is a signature mismatch on chain, so a finalized extrinsic proves the tuple encoded correctly for each runtime. Run from a scratch project outside the repo, so no secret and no repo dependency was involved; successful **registration** belongs to Phase 3.
- **Risks / failure modes**:
  - PreviewNet is post-wipe and volunteer-run: it can be down or re-wiped. Retry, do not redesign.

## Phase 2: containerize the edge (still one environment) ✅ done 2026-08-04

The riskiest step, taken while there is only one env so a failure has exactly one cause.

- **Scope checklist**:
  - [x] Create the external docker network; give every service a per-env network alias (`device-attestation-api-${ENV_ID}`, …).
  - [x] Remove every published host port from `docker-compose.yml`; add an optional debug overlay file that republishes them on `127.0.0.1` when needed (`docker-compose.debug.yml`, and the boundary script now fails on any published port in the base file).
  - [x] Promote the compose `gateway` to the production edge: own compose project, 80/443, a named certs volume, the real domain, and container-alias upstreams. It still holds no secrets (`scripts/verify_compose_boundaries.sh`).
  - [x] Documented the **one-time cutover** in `docs/deploy.md` (recreate services → `systemctl disable --now caddy` → start the edge → verify), with the rollback beside it. CI deliberately **skips** the edge project until it exists, so a merge can never fight host Caddy for 80/443.
  - [x] Run that cutover on the host (done 2026-08-04, before merging the branch — the primary checkout was put on the branch commit so the `edge` project existed before CI could deploy it).
  - [x] Rewrite `docs/deploy.md` (apt install, `chmod o+x /home/ubuntu`, `cp` + `systemctl reload`, the `journalctl -u caddy` and `/docs` 403 troubleshooting rows) and `docs/runbook.md` in the same change.
- **✅ Demo (passed 2026-08-04)**: `https://identity.dotspark.app` serves `/readyz` (`chain: up`, `db: up`), `/api/v1/attester` (200), username search (200), and `/docs` (200) through the `edge-caddy-1` container on a Let's Encrypt certificate issued at cutover; host Caddy is `disable --now`; `sudo docker ps` shows 80/443 on the edge and no published port anywhere else.
- **Risks / failure modes**:
  - Certificate acquisition during the 80/443 handover — keep the window short and the host-Caddy rollback one command away; repeated flapping risks ACME duplicate-certificate limits.
  - The certs volume is now state: name it explicitly and note it in the runbook's backup section.
  - Losing the host-side `curl 127.0.0.1:808x/readyz` habit — that is what the debug overlay is for.

## Phase 3: the PreviewNet environment, on its own hostname ✅ done 2026-08-04

- **Scope checklist**:
  - [x] Second checkout + compose project (`-p identity-previewnet`) with its own `.env` (one checkout per environment — see Approved architecture → Host layout): PreviewNet RPC, fresh `JWT_ED25519_SECRET`, `PEOPLE_NETWORK=paseo`, `QUEUE_ENABLED=false`, `DOTNS_GATEWAY_ENABLED=false`, Phase 0's signer material. No `POOL_TARGET_SIZE`: the ticket pool is not deployed here.
  - [x] Only the per-env services: `device-attestation-api`, `device-attestation-chain-writer`, `username-indexer` (see Approved architecture for why `invite-tickets` is excluded).
  - [x] DNS A record for `identity-previewnet.dotspark.app` at the server IP, unproxied.
  - [x] Add a second site block to `gateway/Caddyfile` reusing the route handles against the `*-previewnet` upstreams, with its own `{$VAR:default}` set; keep it one file (CI installs it, and the edge mounts it read-only). The handles moved into a `(routes)` snippet imported by both sites, so the ownership map exists once.
  - [x] Point the shared `turn-api` / `notify-relay` at a merged `JWT_JWKS_JSON` holding both envs' public keys. Note the accepted consequence: `JWT_JWKS_JSON` is one variable read by every verify-only service in the primary `.env`, so Paseo's indexer and ticket services now also accept PreviewNet-issued tokens. Harmless while both environments run `AUTH_ENABLED=false` (anyone can already mint a token for either); scope it to the shared services before real attestation is turned on.
  - [x] Runbook and deploy commands name their environment wherever it is ambiguous: the established stack uses the project name declared in `docker-compose.yml`, and a second environment passes `-p identity-previewnet` with its own env file. `docs/deploy.md` gained **One-time cutover from host Caddy** and **Adding an environment**.
- **✅ Demo (passed 2026-08-04)**, against the live hostnames:
  - auth handshake → `POST /api/v1/usernames` → `202`, and the outbox row for `pnsmokektfzx.44` reached `ASSIGNED` with a tx hash — a real registration signed by the writer through the shared attester proxy, on PreviewNet People;
  - both environments report `chain: up` against **their own** chain, and their indexers are provably separate projections (37,370 assigned usernames on Paseo at block ~2,208,029; 174 on PreviewNet at block ~65,361, including the name just registered);
  - the shared `turn-api` on a merged JWKS issued credentials for both issuers (201/201) and rejected an unrelated Ed25519 signer (401);
  - `https://identity.dotspark.app` stayed healthy throughout, and both hostnames hold their own Let's Encrypt certificate.
  - Known, pre-existing: `/api/v1/usernames/search` returns no rows for these names on **either** environment — the projection is correct, but the query's `get_byte(identifier_key, 0) = 0` filter (the pre-RFC-0004 carve-out in `crates/username-indexer/src/search.rs`) excludes them. Not caused by this plan.
- **Local rehearsal (passed 2026-08-03)** — the same checks on a laptop before the server had either environment, kept for the two things the live run does not cover (cross-issuer 401s and unknown-`Host` behaviour):
  - both stacks healthy against **their own** chain (`/readyz` `chain: up` on each);
  - the indexer route returned each environment's own projection — `loadcqdojnaia.1` on Paseo, `loadtestvua.1` on PreviewNet — proving per-env chain reads, not just routing;
  - a **shared** `turn-api` with a merged JWKS issued credentials for tokens from **both** issuers (201/201) and rejected an unrelated signer (401), so the shared-service decision is verified rather than assumed;
  - issuer isolation holds in both directions: each `device-attestation-api` returned 401 for the other environment's token and got past auth (400 on a deliberately invalid body) for its own;
  - stopping the PreviewNet environment left Paseo fully healthy (its hostname 502s alone);
  - an unknown `Host` reached no environment at all (empty response, no upstream dialled).
  - Not covered locally: the registration extrinsic (needs the attester proxy secret, which stays on the server) and certificate issuance (needs the real hostnames on 443).
- **Risks / failure modes**:
  - Certificate issuance for the new hostname needs DNS to have propagated first; the edge logs the ACME failure loudly if not.
  - First indexer boot does a full finalized scan; budget it against the 300 s health `start_period` (and CI's 420 s wait) and record the duration.

## Phase 4: one push ships both ✅ code-complete (awaiting the first two-env deploy)

- **Scope checklist**:
  - [x] `deploy.yml` deploys Paseo first, then PreviewNet, then brings the edge up and prunes images once — all under the **single** existing host concurrency group (separate groups would let two runs race one docker daemon and one Caddy config). The remote half is a heredoc'd bash script (loop over `<project>:<checkout>` entries, `docker network create` made idempotent, an env with no checkout is skipped rather than failing) so it can be syntax-checked outside CI.
  - [x] Health checks for both hostnames; a hostname that does not resolve yet is skipped, and the PreviewNet environment is deployed after Paseo so a failure there cannot block or revert it.
- **✅ Demo** (pending the first real deploy — deliberately not run yet): one deployable push to `main` updates both stacks and both health checks pass in the workflow log.
- **Review finding (2026-08-03)**: the established stack **keeps the project name declared in `docker-compose.yml`** (`identity-backend-v2` at the time). An earlier draft passed `-p identity-paseo-next-v2`, which would have created a second project — fresh volumes, an apparently empty database, and a second writer on the same chain. `ENV_ID` still follows the ecosystem ids; the project name and the environment id are separate things.

  *Superseded 2026-08-17.* The DUB rename landed before any of this was deployed, so there were no volumes to orphan and no running writer — the constraint above had nothing to bind. With that gone, the projects are named for their environments (`dub-paseo-next-v2`, `dub-previewnet`), and `docker-compose.yml` declares the primary one as its default. The finding's rule still holds for any **live** host: renaming a project there costs exactly what this paragraph describes.
- **Ordered, not transactional**: if the PreviewNet environment fails to come up, Paseo is already updated and the edge is not — the run fails loudly rather than rolling anything back.

## Phase 5: hand off

- **Scope checklist**:
  - [ ] Tell Leo + Alberto in the project room: the new hostname and the current auth mode.
  - [ ] Update PreviewNet's `backend` in `paritytech/triangle-status/environments.json` (still the legacy `https://polkadot-app-stg.parity.io`).
  - [ ] Flag to Brevity that `HostConfig.preview.identity.backendUrl` should point at the new hostname (it is the legacy staging host today, `host_config.dart:372`), and that PreviewNet is gated off by `kPreviewNetEnabled = false`.
- **✅ Demo**: Brevity switched to PreviewNet completes registration against the shared URL.

# Contracts (guardrails)

- `https://identity.dotspark.app` keeps serving Paseo Next v2 unchanged through every phase.
- An environment is only ever reached through its own hostname. No route, header, or path may fall back to a default environment.
- One writer per nonce lane, where a lane is **(signing account, chain)**: `device-attestation-chain-writer` and `invite-tickets-pool` stay single-instance, and no two submitters share an account **on the same chain**. The two environments' writers deliberately share one account across *different* chains — nonces are per chain, so that is safe and must not be "fixed".
- No shared state between envs: separate databases, volumes, and JWT signing seeds. `device-attestation-api` and `username-indexer` keep separate databases within an env.
- Secrets stay service-scoped: the JWT signing seed only in `device-attestation-api`, public key/JWKS elsewhere, and the gateway holds none.
- `chain-types` stays the only place chain types live; one tuple serves every runtime.
- The route table stays a committed artifact — never hand-edit the running config.
- Nothing but the edge publishes a host port.

# Testing

- Manual smoke demos per phase (above).
- Regression tests only for contracts: extension-set resolution against both runtimes; `verify_compose_boundaries.sh` after the networking change; cross-env token rejection; `just check` before each phase closes.

# Polish rounds (after MVP)

## Polish round 1: repeatable post-wipe provisioning

- Turn Phase 0 into a re-runnable sequence with a named owner.
- ✅ Check-in demo: re-provision the env from scratch against a re-wiped PreviewNet using only the documented steps.

## Polish round 2: edge observability

- Access logs split by hostname; a documented way to answer "which env answered this request".
- ✅ Check-in demo: from logs alone, attribute a sample request to its environment.

# Later / Deferred

- **DevNet** (`products-devnet`, public community Paseo). Needs subxt's real `CheckMetadataHash` in the tuple and an attester authority on a chain PCF controls. Revisit once these two envs are stable and the attester question has an owner.
- Multi-topic push: `notify-relay` has a single `APNS_TOPIC` (a bundle id), so Nova's separate builds cannot share one relay. Per **app**, not per env — its own slice when Nova needs push through V2.
- **The legacy-origin escape hatch is gone.** `/api/v1/subscriptions*` is the one removed path a shipping client has ever referenced (Android, feature-gated and deferred out of V1 — see `docs/plans/done/notify-relay.md`). Re-enabling Android subscription sync therefore needs notifications v3, not a Caddy variable. Deliberate: nothing was ever configured behind `LEGACY_BACKEND_ORIGIN`, so it was a hatch to a service that did not exist.
- **`invite-tickets` on PreviewNet**, which needs a second `Any` delegate on the shared authority so the pool does not share the chain-writer's nonce lane. Revisit if game/personhood flows are wanted there — and note the Paseo pool needs fixing first (it runs the unfunded `//Bob` compose default today, so `/api/v1/invitation-ticket/claim` answers 422 there).
- A real `previewnet` variant of the `PEOPLE_NETWORK` enum (enum + config + migration relaxing the `CHECK` + `just openapi`). Revisit if the label ever becomes load-bearing or a client rejects `paseo` on PreviewNet.
- Moving an env onto its own machine — revisit if the host passes ~50% memory or the indexer's disk growth becomes the constraint.
- Turning on real attestation (`AUTH_ENABLED=true`) on the new env — follows the attestation plan, not this one.
