# Goals

Two milestones, kept distinct (this was the main plan fix):

- **Core MVP (M0)** — a Rust `identity-service` that a shipping iOS build can hit on a testnet to: get a challenge → obtain a JWT → check availability → **register a username on People Chain**, confirmed on-chain. Attestation-off, iOS-first, Android contract-compatible. The old backend keeps serving everything else during this phase.
- **V1 (Retirement Release)** — the app's **whole live surface** rebuilt in the Rust workspace (identity-service + the thin siblings + gateway), attestation hardened, parity-or-better, so the **old backend is fully decommissioned**. This is "V1 done" per the design doc, not the Core MVP.

Cross-cutting:
- Byte-compatible with the shipping iOS/Android clients on every app-used `/api/v1` route — same paths, same payload shapes, zero client changes to cut over.
- **Parity-or-better bar (non-negotiable):** for every endpoint V1 exposes, it must **meet or beat the current deployed backend** on latency, throughput, and reliability. The current backend **batches** registrations, so V1 must batch too (see Contracts).

# Milestones

- **Core MVP (M0):** Slices 1–5 below. `identity-service` only, attestation-off. Demoable: iOS registers a name on a testnet. **Old API still serves the non-core routes.**
- **V1 (Retirement):** Slices 6–9. Attestation-hard; the thin siblings (`/notify` relay, `/turn/issue` cred service, invite faucet, `username-read`) built **in this workspace** (not other teams); single-URL gateway; deployment; parity benchmarks; cutover → **retire the old API.**
- **Post-V1:** metrics/ops polish, notifications v3 (revive `/subscriptions`), Play Integrity self-managed keys, writer scaling, and the on-chain moves (availability + discriminator, invite graph, decentralized TURN).

# Non-goals

- **Not in Core MVP** (but **in V1 retirement**, built thin in this workspace): `/notify` relay, `/turn/issue` cred service, `/invitation-ticket/claim` faucet, `username-read`. The old backend serves these during M0 only.
- **Removed entirely** (not rebuilt): `/subscriptions*` (confirmed unused — the Android sync code is commented out, iOS never called it; revived in notifications v3), statement pipeline (already decentralized).
- `payment-status` / `registration/queue` / voucher **endpoints** — forward-spec with no shipping caller; deferred. (The free-registration **queue behavior** inside `POST /usernames` **is** in scope — see Slice 4.)
- **Proof-of-compute** — deferred; no shipping client implements it (verified against latest iOS + Android). V1 handshake anti-abuse = rate-limiting.
- Play Integrity self-managed-keys optimization — deferred (Post-V1); V1 uses Google's verify API.

# Design principles

- **User journey drives order** — unlock "app authenticates and registers a name on a testnet" as early as possible (Core MVP).
- **Attestation-off first** — run the Core MVP with `AUTH_ENABLED=false`; attestation-hard lands in V1 (before old-API retirement — you can't replace prod without real verification).
- **`who` == JWT subject** — registration binds the username to the authenticated account; reject a body beneficiary that differs from the JWT subject (hard contract).
- **Cross-platform parity** — core routes are platform-neutral; keep Android contract compatibility from Core MVP.
- **Chain is reconciled to the outbox, never the reverse** — one live signer per account; `SUBMITTING` recorded before await; `AlreadyRegistered` = success.
- **Ugly but functional** over polished-but-incomplete.

# User journey

1. App boots and requests an attestation challenge.
2. App exchanges the challenge (+ platform attestation, no-op while off) for a JWT.
3. App checks whether a desired username (and digit suffixes) is available.
4. App submits a username registration; backend reserves it and submits it on-chain.
5. App confirms the result by reading People Chain (`UsernameOwnerOf`) — no backend poll route.

# Foundations / Already shipped (✅)

## Rust workspace + identity-service health server
- What exists: Cargo workspace (`crates/identity-service`, `crates/chain-types`), axum server with `/healthcheck` `/livez` `/readyz`, `Config` reading `BIND_ADDR` / `AUTH_ENABLED` / `ENFORCE_AUTH` (logs attestation posture), `justfile` gate (fmt / clippy `-D warnings` / test), `deny.toml`, `rust-toolchain.toml`. Committed `5892ac9`.
- ✅ Demo: `just run` then `curl :8080/healthcheck` → `{"status":"ok",...}`; `just check` is green.
- Gaps: no chain, auth, DB, or username routes yet.

## Reference specs (grounded, not to re-derive)
- What exists: `docs/design-doc.md` (canonical, committed); memory notes `Identity Backend V2 — Auth & Username Registration Spec`, `… Doc Digest`, `… Push Notifications Plan`, `Identity Backend — Outage Postmortem 2026-01-28`.
- ✅ Demo: open the Auth spec note — the exact challenge → attestation → JWT → username-claim flow.

# Pre-flight gates (before any build beyond the scaffold)

Per Karim/Ed (design-doc-first) and the project rule "analysis/design until a plan is approved".

- [ ] **Design approved** — `docs/design-doc.md` circulated; Torsten + Karim sign-off (approvers); Ed's design-doc-first gate. Alberto is co-owner — coordinate in the IB v2 channel (`!LEzxPgpbmVlWXCAnzo`).
- [ ] **Cutover memo** — for each app-used route, its temporary provider during M0 (old backend), its V1 owner (a thin service in this workspace), DB + secrets boundary, and cutover behavior. Feeds Slice 9.
- [ ] **Decide-early items** locked (see "Key decisions" below).

# Core MVP slices (M0 — ship-shaped, demoable)

## Slice 1: `chain-types` — subxt 0.50 codegen ✅
- **Goal**: a typed People Chain surface the rest of the workspace builds against.
- **Scope checklist**:
  - [x] Decide + record the testnet target and its People Chain RPC. **Decided: Paseo Next v2** (`wss://paseo-people-next-system-rpc.polkadot.io`, parachainId 1502) — the forward path and the env whose `identity-backend-next` V1 replaces. PreviewNet (`wss://previewnet.substrate.dev/people`) is the fallback.
  - [x] Vendor People Chain metadata into the repo — `crates/chain-types/metadata/people.scale` (V16, trimmed to `System,Balances,Utility,Proxy,People,PeopleLite,Resources`). Pinned to `next-people-paseo` **specVersion 1000029**, txVersion 3.
  - [x] Generate the `#[subxt::subxt]` module; exposes the registration/attestation calls (`Resources::register_lite_person`/`register_person`, `PeopleLite::attest`), reads (`Resources::UsernameOwnerOf`, `UsernameReservationQueue`, `ReservationOf`, `PeopleLite::AttestationAllowance`), and the signing levers (`Proxy::proxy`, `Utility::force_batch`).
  - [x] Add subxt 0.50 + subxt-signer to the workspace deps (`subxt` with `default-features = false, features = ["native"]` for offline codegen; `subxt-signer` declared for the identity-chain-writer).
- **✅ Demo**: `cargo build -p chain-types` passes; two unit tests construct the `Resources::UsernameOwnerOf` storage query and the `register_lite_person` call from the generated API. `just check` (fmt + clippy `-D warnings` + test) is green.
- **Risks / failure modes**: metadata drift vs the live testnet — metadata is pinned to specVersion 1000029; refresh command is documented in `crates/chain-types/src/lib.rs`. ⚠️ Paseo Next v2 had a post-upgrade `InvalidAttestationSignature`/tx-extension churn (People Chain note) — revisit signed-extension params at the signing boundary (Slice 4), not in this leaf crate.

## Slice 2: Auth handshake (attestation-off) + ops/persistence foundation ✅
- **Goal**: app gets a JWT end-to-end with `AUTH_ENABLED=false`, on a real DB with fail-fast startup.
- **Scope checklist**:
  - [x] `POST /api/v1/auth/challenges` — issues + persists a single-use, TTL'd challenge (DB-backed, base64 opaque blob). Returns `201 {challenge}` (matches the reference backend's status).
  - [x] `POST /api/v1/auth/app-attest/attestations` — iOS App Attest registration. **Shape-preserving no-op returning `202 {}`** while `AUTH_ENABLED=false` (body `{keyId, challenge, attestation}` per the shipping iOS caller).
  - [x] `POST /api/v1/auth/token` — verifies challenge (single-use/TTL) + sr25519 client proof over `SHA256(challenge ‖ clientId ‖ SHA256(rawBody))` (via `subxt_signer`, symmetric with real wallets), issues JWT (`accountId`/`sub` = `0x`+hex(clientId), `appFromOfficialStore`, `plt`, `iss`, `iat`, `exp`), returns `{token, refreshToken}`. Raw body read as `Bytes` so the proof hash matches. Attestation (`Auth-Payload`/`attestationChain`) is a no-op while off.
  - [x] `POST /api/v1/auth/token/refresh` — rotate; exposes the **single** clean path (the prod double-mount is not reproduced; confirmed against shipping iOS + Android). Single-use rotation in a transaction (`FOR UPDATE`, mark spent + mint replacement).
  - [x] JWT signing = **asymmetric EdDSA/Ed25519 + JWKS** (`GET /.well-known/jwks.json`, OKP JWK); refresh-token store carries claims for re-mint.
  - [x] **Persistence foundation**: sqlx + embedded generated migrations (`sqlx::migrate!`, `0001_auth.sql`); DB-backed challenges + refresh tokens. ⚠️ Queries are **runtime-checked** for now (no local Postgres/sqlx-cli); compile-time `query!` + `cargo sqlx prepare` is a follow-up once CI has a Postgres.
  - [x] **Ops foundation**: fail-fast validation of required env/secrets (`DATABASE_URL`, `JWT_ED25519_SECRET` → must decode to 32 bytes) — startup exits `1` with a clear cause; structured tracing (per-request method/path/status/latency); minimal per-route/per-IP fixed-window rate limits on the auth endpoints.
- **✅ Demo (verified 2026-07-06)**: against a Docker Postgres, `examples/auth_smoke.rs` drives challenge → token → refresh with a `//Alice` sr25519 proof; the JWT decodes to `accountId=0xd435…` (Alice's pubkey) + `appFromOfficialStore=true`; `/readyz` reports `db: up`; JWKS serves the Ed25519 key; startup **fails loudly** on each missing/invalid secret; `just check` is green (JWT + proof round-trip unit tests pass). Run: `cargo run -p identity-service --example auth_smoke`.
- **Risks / failure modes**: refresh semantics match the shipping clients (`{refreshToken}` → `{token, refreshToken}`); challenge single-use is now DB-enforced (real, not best-effort) — cross-replica safe. Rate limiter is per-instance in-memory (Cloudflare edge + per-JWT limiter are Post-V1).

## Slice 3: Username read path ✅
- **Goal**: availability + attester key + search, backed by chain reads.
- **Scope checklist**:
  - [x] `GET /api/v1/attester` — returns `{attester: "0x"+hex}` from config `ATTESTER_PUBLIC_KEY` (public, matches the reference backend).
  - [x] `POST /api/v1/usernames/available` — JWT-gated; chain-backed against `Resources::UsernameOwnerOf`. Body `{usernames:[base]}`, `?version=v1` response `{_tag:"v1", value:{base:{status, availableDigits?}}}`; `AVAILABLE|EXHAUSTED|INVALID`. Reads all 100 `{base}.{NN}` discriminators concurrently against one block snapshot; `availableDigits` = `01..99` minus taken; `INVALID` for base < 6 chars / non-lowercase; `EXHAUSTED` when all 100 owned.
  - [x] `GET /api/v1/usernames/search` — **kept public + rate-limited** (⚠️ deviation from the plan's "JWT-gated" wording: the shipping **iOS** client calls search **unauthenticated**, so JWT-gating breaks byte-compat — confirmed against `polkadot-app-ios-v2`). Returns the correct shape; results are empty until the prefix indexer lands in Slice 7 (`username-read`).
- **New foundation**: subxt `OnlineClient` People Chain read client (`chain.rs`, `jsonrpsee` feature, `PEOPLE_RPC_URL`), a JWT bearer verify extractor (`extract.rs` + `Jwt::verify`), `readyz` now gates DB **and** chain.
- **✅ Demo (verified 2026-07-06 vs live Paseo Next v2)**: `available` for `tqegciilc` → `AVAILABLE` with digit **1 excluded** (its `.01` is owned on-chain), free base `zzzxqwvb` → all 99 digits, `abc` → `INVALID`; `available` without a bearer → `401`; `/attester` returns the key; `/readyz` → `chain: up`. Extended `examples/auth_smoke.rs`; `just check` green.
- **Risks / failure modes**: 100 chain reads per base (concurrent, one snapshot) — fine for M0; on-device availability + a standalone indexer are Post-V1 / Slice 7. Metadata drift → the static storage address validates against live metadata (`IncompatibleCodegen` if the runtime upgrades → re-vendor).

## Slice 4: Username write path (the core unlock)
- **Goal**: `POST /api/v1/usernames` registers on People Chain via the free-reg queue + outbox + a single identity-chain-writer.
- **Scope checklist**:
  - [x] `POST /api/v1/usernames` validates + reserves → `202`; body per current contract (flat: `candidateAccountId`, `username` (base only), `preferredDigits?`, `candidateSignature`, `ringVrfKey`, `proofOfOwnership`, `consumerRegistrationSignature`, `identifierKey`, optional `dotns`). Response `202 {base_username, digits, username, device_check_available?}`.
  - [x] **`who` == JWT subject (hard contract + test)**: rejects (`403`) when `candidateAccountId` (SS58-decoded) differs from the authenticated JWT subject; the account's own signatures still travel to chain. Unit-tested digit/base/preferred logic; SS58↔subject check in the handler.
  - [~] **Free-registration eligibility + queue**: intake reserves instantly under `AUTH_ENABLED=false` (M0). The iOS DeviceCheck free-slot gate (`200 {registrationOutcome:"PAYMENT_REQUIRED"}`) is a **Slice 6** concern (needs attestation on) — seam noted in `register.rs`. Digit selection (preferred → `409` if taken, else random excl. `00`) matches the current backend.
  - [x] Reservation outbox table + state machine (`RESERVED → SUBMITTING → ASSIGNED | RETRY_AFTER | FAILED_TERMINAL`) per `identity-chain-writer-outbox-spec.md` (`migrations/0002_usernames.sql`, `outbox.rs`).
  - [x] Chain-writer runs as its **own single-instance binary** (`identity-chain-writer`), separate from `identity-api` (identity-service is now a lib + two bins). Minimal **Postgres lease** (`writer_lease`: `holder_id`/`lease_epoch`/`expires_at`/heartbeat); every writer DB mutation guarded by `WHERE … lease_epoch = current`.
  - [x] Sign via a **`Proxy` of a cold primary** (attester authority) when `CHAIN_WRITER_PROXY_FOR` is set — `Proxy.proxy(Id(real), None, PeopleLite.attest(…))` built with subxt dynamic values (flow-funder pattern). **One nonce lane**; no pipelined nonces. Real multi-registration batching is a V1 parity task in Slice 8.
  - [x] Chain-writer submits via subxt 0.50; **records `SUBMITTING` (tx_hash+nonce) before await**; `AlreadyRegistered` = success; reconciles unknown outcomes against `UsernameOwnerOf`; drains `SUBMITTING` on lease-acquire; cached nonce reset on any error. ⚠️ **mortal extrinsics deferred** (immortal for now; reconcile-by-chain-state + attempt cap is the active safety net) — hardening follow-up.
  - [x] **Avoid masked inner-call failures**: submit `PeopleLite.attest` directly while each extrinsic carries only one registration. When real multi-registration batching lands, inspect each batch item's result before advancing its row. Covered by direct + proxied payload-shape tests.
  - [x] `readyz` gates on Postgres **and** People Chain connectivity (landed in Slice 3).
- **✅ Demo (verified 2026-07-10)**: the local compose stack accepted a generated real-signature payload (`202`), the direct proxied `PeopleLite.attest` finalized, and `directzdxab.47` reached `ASSIGNED` on Paseo Next v2. On-chain checks confirmed `UsernameOwnerOf`, `Resources.Consumers`, and the automatic Statement Store allowance (`max_count=50`, `max_size=512000`). `just check` is green. This proves the service flow, not yet a shipping-device smoke; real attestation gating is **Slice 6**.
- **Risks / failure modes**: nonce collisions (mitigated by single writer + one nonce lane; the lease shrinks deploy overlap; chain nonce + reconcile is the real backstop); RPC drops (reconnect + reconcile, never blind resubmit).

## Slice 5: Test on the real apps (Core MVP done)
- **Goal**: a shipping **iOS** build drives the whole flow end-to-end against `identity-service` on a testnet, and **Android** route/contract compatibility is verified — both under `AUTH_ENABLED=false`.
- **Scope checklist**:
  - [ ] Point an iOS build's backend base URL at the Rust service (testnet, `AUTH_ENABLED=false`); run challenge → token → available → register → on-chain confirm.
  - [x] Registration regression check (local real-signature harness): after finalization, `UsernameOwnerOf`, `Resources.Consumers`, and the automatically granted Statement Store allowance exist on-chain.
  - [ ] Route-shape **golden tests** for the core auth/username/attester routes covering both iOS and Android request/response shapes.
  - [ ] At least one **Android** smoke against the service under `AUTH_ENABLED=false`.
  - [ ] Diff any payload/shape mismatch vs the shipping clients; fix in `identity-service`, not the apps.
  - [ ] Failure-path test: force `PeopleLite.attest` to fail; verify the row is retried/failed and never marked `ASSIGNED`.
- **✅ Demo (Core MVP done):** username registered from the iOS app appears owned on People Chain with no app-side changes; Android golden tests pass and an Android smoke authenticates. **Old backend still serves the non-core routes.**

# V1 Retirement slices (retire the old API)

## Slice 6: Attestation hard — iOS + Android together
- **Goal**: real provider verification, so the new backend can actually replace prod.
- **Scope checklist**:
  - [ ] iOS App Attest verify + DeviceCheck uniqueness.
  - [ ] Android Play Integrity (**Google verify API** in V1) + vanilla-APK TEE (`POST /api/v1/auth/android/attestation`, cert-chain + `KeyDescription` checks).
  - [ ] Flip `ENFORCE_AUTH` soft → hard per environment.
- **✅ Demo**: bad/missing attestation rejected with `403 INTEGRITY_FAILED` in hard mode; Android registers on a testnet.

## Slice 7: Thin sibling services (in this workspace)
- **Goal**: build the remaining app-called surface as thin, isolated, JWT-verify-only siblings — owned here, not by other teams.
- **Scope checklist**:
  - [ ] `/notify` relay (thin P2P → APNs/FCM); its own DB/secrets.
  - [ ] `/turn/issue` credential service (JWT-gated; coturn relay stays SRE infra).
  - [ ] Invite faucet (`/invitation-ticket/claim`) — today's faucet only; no game/personhood logic.
  - [ ] `username-read` — decoupled availability + prefix-search indexer from finalized People Chain state (may fold into identity-service for M0, split here).
  - [ ] `auth-verify` leaf crate + JWKS endpoint so every sibling verifies tokens with the public key (no core-table reads).
- **✅ Demo**: each sibling validates an identity-service JWT and serves its route independently; killing one doesn't affect identity-service.

## Slice 8: Gateway + deployment + parity
- **Goal**: one public URL and a deployable stack that meets the parity bar.
- **Scope checklist**:
  - [ ] Single-URL gateway (path-routing `/api/v1/*` to the owning service).
  - [ ] Dockerfile + CI helm validation (`helm lint` / `helm template --debug`); gated prod config (CODEOWNERS on prod values) — from the 01/28 postmortem.
  - [ ] **Compile-time SQL — flip the switch (deferred from Slice 2; do it here, once CI has a Postgres).** Move from runtime-checked `sqlx::query` to compile-time `query!`/`query_as!`:
    - Stand up a `docker compose` Postgres for local dev + a Postgres service in CI (`DATABASE_URL`); install `sqlx-cli`.
    - Convert the `format!`-built SQL first — `chain/outbox.rs` `SELECT_COLS` / `LEASE_HELD` — to string literals; `query!` cannot take a runtime-built string.
    - Switch queries to `query!`/`query_as!` (deletes `chain/outbox.rs::row_to_reservation` + the hand-written `.try_get` mapping); catches schema/type drift at build time.
    - `cargo sqlx prepare` → commit `.sqlx/`; CI sets `SQLX_OFFLINE=true` and runs `cargo sqlx prepare --check` to fail on a stale cache. Routine `cargo build` / rust-analyzer stay **DB-free** off the committed cache — a live DB is only needed to (re)generate the cache when a query changes.
    - Add `#[sqlx::test]` coverage (isolated per-test DB, auto-migrated) for outbox state transitions + auth refresh rotation.
  - [ ] **Parity-or-better benchmarks**: p50/p95 for `token` / `available` / `register` / `search` + registration throughput ≥ current prod.
  - [ ] **Writer safety monitoring before cutover**: alert when registrations remain `RESERVED` / `SUBMITTING` too long or repeatedly fail finalization.
- **✅ Demo**: all routes reachable via the single URL through the gateway; benchmarks meet or beat prod.

## Slice 9: Cutover + retire the old API
- **Goal**: switch traffic to the Rust workspace and decommission the old backend.
- **Scope checklist**:
  - [ ] **Data migration decision**: migrate V1 data or wipe & start fresh (the biggest open question — decide with the team).
  - [ ] Per-route cutover order + rollback plan; shadow/contract tests against real client calls.
  - [ ] Old-backend shutdown criteria + execution.
- **✅ Demo (V1 done):** all app-called routes served by the Rust workspace behind the gateway; old backend retired.

# Contracts (guardrails)

- Every app-used route keeps **exact** paths + request/response shapes; expose the single clean `/api/v1/auth/token/refresh`.
- **`who` == JWT subject** on `POST /usernames` (reject mismatch; account signature still required).
- `AUTH_ENABLED` (no-op attestation when false) and `ENFORCE_AUTH` (soft vs hard) honored.
- One live signer per chain account; `SUBMITTING` recorded before await; `AlreadyRegistered` = success; unknown outcome ⇒ reconcile against `UsernameOwnerOf`; outbox row is source of truth.
- `#![forbid(unsafe_code)]`; `just check` (fmt + clippy `-D warnings` + test) stays green.
- Generated-only migrations; no manual edits; identity-service owns no ejected tables.
- **Parity-or-better vs the deployed backend** (hard gate before cutover): benchmark prod p50/p95 + registration throughput as the baseline; V1 must meet or beat each. Prod batches registrations (`force_batch`), so V1 batches too.
- **Ops floor (postmortem-driven):** fail-fast on missing required env/secrets at startup; `readyz` reflects real DB + chain health; structured logging; minimal per-JWT/route rate limits on auth + register + search; writer/outbox/finalization safety metrics and alerts land before cutover. (Broader product metrics + Cloudflare edge rules come Post-V1.)

# Key decisions (decide early)

- **Testnet target + People Chain RPC** for codegen + testing — ✅ **decided: Paseo Next v2** (`wss://paseo-people-next-system-rpc.polkadot.io`), PreviewNet as fallback (Slice 1).
- **DB layer**: ✅ **decided: sqlx + embedded migrations** (`sqlx::migrate!`). Runtime-checked queries for now; compile-time `query!` + `cargo sqlx prepare` deferred until CI has Postgres (Slice 2) — concrete flip-the-switch steps in **Slice 8**.
- **subxt**: static codegen from vendored metadata (preferred) vs dynamic. ✅ static (Slice 1).
- **JWT scheme**: ✅ **implemented: asymmetric EdDSA/Ed25519 + JWKS** (`ed25519-dalek`, JWKS at `/.well-known/jwks.json`); token stays opaque to the app (Slice 2).
- **Attester + identity-chain-writer signing accounts**: how keys are provided/funded on the testnet (proxy per worker; never share an account).
- **Cutover & data migration**: migrate vs wipe (Slice 9) — the biggest open decision.

# Testing

- Manual smoke demo per slice (the ✅ Demos above).
- Regression tests for contracts: JWT claim shape, `who`==subject rejection, free-reg decision (`INSTANT`/`QUEUED`/`PAYMENT_REQUIRED`), outbox state transitions (incl. `AlreadyRegistered`=success + reconcile), failed `PeopleLite.attest` handling, finalized registration postconditions (`UsernameOwnerOf` + `Resources.Consumers` + Statement Store allowance), and route-shape golden tests — extended from core routes (M0) to the full app surface (V1).

# Post-V1

- **Metrics + edge ops:** Prometheus `/metrics`; per-JWT backend rate limiter tuned per replica; Cloudflare edge rules.
- **Notifications v3:** revive `/subscriptions`(+rules) / the fan-out per the push-v3 seed doc.
- **Play Integrity self-managed keys:** local Classic-token verification (AES decrypt + EC verify) to escape the 10k/day cap; Google-API path kept as rotation fallback.
- **Writer scaling:** proxy-account sharding + batching / multi-nonce when volume demands.
- **On-chain moves (Torsten's "move to chain what can move"):** username availability + discriminator on-chain (removes the backend index dependency); invite graph on-chain (drops the custodial key pool); decentralized/host-managed TURN; speculative backend-less on-chain registration.
- **Proof-of-compute** handshake gate — once a client implements it.

---
_Source of truth for shapes/flows: `docs/design-doc.md` and the memory notes in `33.05 Identity Backend V2/`. Ship-first plan generated 2026-07-02; milestone split + gap fixes 2026-07-03._
