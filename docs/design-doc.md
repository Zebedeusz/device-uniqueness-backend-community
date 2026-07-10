# Identity Backend V2 — Rust Rewrite

Design doc
Authors: Talles Borges, Alberto Nicolás Penayo
State: draft
Approvers: Torsten, Karim
Last updated: 2026-07-10

## TL;DR

The current identity backend is one TypeScript service doing too much — identity, notifications, TURN, and invites all share code, secrets, and a failure domain, so an unrelated bug can take down login and username registration. V2 rewrites it as a **Rust Cargo workspace of small, isolated services** — each independently deployed with its own persistence when needed, coupled only by a shared JWT contract and a thin core of genuinely-shared code. It stays **100% API-compatible** with today's mobile clients (same base URL, same paths, no client changes) and lets us **fully retire the old backend**. The result is a smaller security boundary, failures that stay contained to one service, and a clean base to move more responsibilities on-chain over time.

## In Short

- **One workspace, many isolated services.** `identity-service` plus thin siblings (username read/search, a `/notify` relay, the invite faucet, TURN creds) live in one Cargo workspace but **deploy independently**. Each owns its persistence if it has any, plus its secrets and operational lifecycle. No shared tables.
- **Minimal coupling by design.** Services do **not** depend on each other at runtime. They share only a thin **core** of real shared code (chain connection, DB connection, config, telemetry) as leaf crates — nothing product-specific.
- **JWT is the only contract.** `identity-service` is the **sole issuer** of JWTs (asymmetric, EdDSA/Ed25519 + JWKS). Every other service is **verify-only** — it validates tokens with the public key, never calls back into core and never reads core's tables.
- **One public URL, path-routed.** A thin gateway routes auth, registration writes, and availability to identity-service; `GET /api/v1/usernames/search` to username-read; `/api/v1/notify` to notifications; and the remaining paths to their owners. No per-service public subdomains. This preserves 100% API compatibility.
- **Migrate the app's whole live surface, then kill the old API.** Every endpoint the app calls today is rebuilt in the workspace so the old backend can be **fully decommissioned** — not left running to serve leftover routes.
- **The tradeoff: more moving parts.** Isolation means more deployables, more databases, a gateway to run, and JWT/JWKS operations across services — plus a careful cutover from V1. We accept that cost for contained failures and a small audit surface; the **cutover is the main risk we manage** (see Open Questions).

## Background / Context

- **V1 did its job for Summit but is too coupled for mainnet.** Identity, push notifications, statement fan-out, TURN credentials, and invite brokering all live in one service, sharing code, secrets, and one deploy.
- **Learned at Summit: cross-team dependencies hurt.** Spreading tightly-related work across separate repos and owners created fragile coupling. The answer is a **single workspace with minimal dependencies** — not many disowned services run by new teams.
- **Clear ownership.** Novasama owns the app; Parity owns the backend. That backend should stay consolidated under one team, not split across many.
- **Reliability.** A push-notification bug, a statement backlog, a TURN misconfiguration, or an invite-pool problem must **never** block login, attestation, or username registration.
- **Security / audit scope.** `identity-service` should hold **only** identity secrets — JWT signing keys, refresh-token records, voucher hashes, attester authority, and platform-attestation credentials — so its audit surface stays small.

## Scope

### In-scope (V1 — migrate the app's live surface to Rust, retire the old API)

Principle: **every endpoint the app calls today is rebuilt in the new workspace so the old backend can be fully decommissioned.** V1 builds:

- **`identity-service`**: its MVP is built for attestation-off auth/JWT, direct-chain discriminator availability, username registration, the reservation outbox, the chain writer, and attester publication. Real device attestation and free-registration eligibility remain required retirement work with dedicated plans.
- **Username availability / search:** identity-service currently serves `/usernames/available` from direct `UsernameOwnerOf` reads. The active retirement path puts `GET /usernames/search` in the independent `username-read` service, populated from finalized `Resources.Consumers` state. Both preserve the shipping client contracts.
- **`notifications`** — thin `/notify` relay only. (The `/subscriptions`(+rules) surface is **confirmed unused** by both shipping clients — the Android sync code is commented out, parked for "v2 push" — so it is **not built for V1**; deferred to notifications v3.)
- **Invite faucet** — slim; today's faucet only (`/invitation-ticket/claim`). **No** backend game / personhood service — all DIM / Game / ProofOfInk logic runs on-chain + app.
- **`turn`** — thin `/turn/issue` **credential** service (JWT-gated). The coturn **relay** stays SRE infrastructure, not ours.
- **Single-URL gateway** routing `/api/v1/*` to the owning service.
- **Removed, not rebuilt:** statement fan-out and the push subscription fan-out (already decentralized via the Statement Store / dead in the app).

Delivery is tracked as independent ship-first plans under `docs/plans/`, indexed by `docs/roadmap.md`. The completed Identity Service MVP is not the retirement release; every remaining route, hardening gate, migration decision, and cutover step must still close before the legacy backend is shut down.

### Out-of-scope (deferred / Future Work)

- Moving username availability / registration or invites **on-chain** — that's Future Work. (Normal username registration still writes to People Chain, as today.)
- **Client-minted / decentralized TURN** — Future Work (needs a Nova app change + a secret-delivery design); V1 keeps the thin cred service for compatibility.
- **Proof-of-compute** — deferred; no shipping client implements it (see Future Work).
- Redesigning People Chain / People Lite pallets, or changing People Lite economics / queue policy (preserve current behavior).
- Decentralizing People Lite itself.
- Choosing permanent infrastructure if the current container deployment can ship the Rust services safely.

## Technical Background

Assumptions and constraints that justify the design (risks are consolidated under Proposed Solution).

### Assumptions

- `subxt 0.50` can cover all required People Chain / People Lite / DotNS / indexing / health operations from checked-in metadata.
- Platform attestation providers (App Attest, DeviceCheck, Play Integrity, Android Keystore) remain available and their verification stays off-chain.
- Chain writes need **nonce coordination**; write throughput must scale to registration volume.

### Constraints

- API frozen from the app's perspective (paths + payload shapes unchanged).
- Owned by Parity's identity-backend team; isolated deployables in one workspace sharing only a thin core.

### Domain fact — genuineness vs uniqueness

- App Attest, Play Integrity, and Android Keystore prove device/app **genuineness**.
- **DeviceCheck** supplies iOS **uniqueness** for free-registration eligibility.
- Android has **no upstream anti-Sybil** signal in scope — eligibility rests on genuineness plus a persistent free-registration consumption record.

### Why Rust

The **truAPI is being rewritten in Rust**, and the host already ships substantial reusable Rust. Choosing Rust aligns the identity backend with the ecosystem's direction and lets us reuse existing Rust rather than maintain a divergent TypeScript stack.

## Proposed Solution

Rewrite the identity backend as a **Rust workspace of isolated, independently-deployable services** sharing only a thin core and a JWT contract, behind a single path-routing gateway. We choose this because the problem is not local code quality — it's an oversized runtime, database, secret, and failure boundary. Isolation (not disowning) fixes that while keeping the whole backend surface owned by one team.

### High-Level Architecture

- **Gateway (thin, routing-only):** one public URL; routes `/api/v1/<namespace>` to the owning service. Preserves the existing API contract.
- **Services (isolated deployables, own persistence when needed + secrets):**
  - `identity-service` — device attestation, the auth handshake + JWT issuance, username **write** path (free-reg queue + queue-advancer + reservation outbox + identity-chain-writer), attester publication.
  - `username-read` / indexer — independent finalized-state projection for assigned-username prefix search. Direct-chain discriminator availability remains in identity-service for the current MVP.
  - `notifications` — thin `/notify` relay (subscription fan-out dropped; `/subscriptions` confirmed unused → notifications v3).
  - invite faucet — slim; issues invite tickets only (no backend game / personhood service).
  - `turn` — `/turn/issue` credential service (coturn relay = SRE infra, not ours).
- **Shared leaf crates only:** `chain-types` (subxt codegen), chain-connection, DB connection, config, telemetry, `auth-verify`. No shared database; no product code shared across services.

Illustrative crate layout:

- `identity-backend-v2/crates/`
  - `identity-service/` — shared domain (auth · username · outbox)
    - bin `identity-api` — axum API, N stateless replicas
    - bin `identity-chain-writer` — one nonce lane per writer, Postgres lease (V1: one active instance; scales via proxy sharding)
  - `chain-types/` — subxt codegen + typed People Chain surface
  - V1 thin bins: `username-read`, `notifications` (`/notify`), `turn` (`/turn/issue`), `invite-faucet`
  - leaf crates: `auth-verify`, `telemetry`, `config`

### Detailed Design — Key Components (high-level)

- **Auth / attestation (identity-service):** stateless TTL-bound challenge → verify platform attestation → verify client proof → issue short-lived JWT access token + opaque refresh token → rotate refresh atomically, reject reuse. Anti-abuse on this **unauthenticated** handshake is **rate-limiting** in V1 (a proof-of-compute gate is Future Work — no shipping client implements it yet).
- **Username write (identity-service):** validate the request — **binding the username to an account the caller provably controls** (`who` must equal the authenticated JWT subject, plus the account's own signature) → resolve eligibility and return exactly one decision:
  - **INSTANT** — a valid voucher (skip the line),
  - **PAYMENT_REQUIRED** — the device already used its one free slot (DeviceCheck / device-id), or an unprovable Android build,
  - **QUEUED** — a fresh, unique device with no voucher: free but **rate-limited**, so it waits.
  Approved registrations land in the **outbox**; a **queue-advancer** promotes QUEUED entries by balance priority; the **identity-chain-writer** drains the outbox to People Chain (`PeopleLite.attest`, optional DotNS reservation). Registration is **async** (`202`); the app confirms the result **on-chain** (`UsernameOwnerOf`). (V1 uses separate daemons for the queue-advancer and identity-chain-writer; V2 may consolidate — an implementation choice.)
- **Username read (own service):** serves `/usernames/search` from a finalized `Resources.Consumers` projection; skips malformed records but logs decode failures. Identity-service keeps the working direct-chain `/usernames/available` path unless a later plan deliberately moves it.
- **Notifications / TURN / invite faucet:** thin siblings — a `/notify` relay, a `/turn/issue` credential service (coturn relay is SRE infra), and the slim invite faucet. All are **JWT verify-only** with **no synchronous dependency** from identity-service.
- **Chain-writer:** V1 runs one active instance (one nonce lane, Postgres lease) for simplicity; signs via a least-privilege **Proxy** of a cold attester authority. Scales to multiple writers via proxy-per-worker sharding (see Alternatives).

### Data Model

- **Per-service persistence** — each service owns its own persistence if it needs any; no shared tables. Stateless services should not add a database without a proven requirement.
- **`identity-service` owns:** platform-attestation evidence, consumed-uniqueness markers, refresh-token state, voucher-secret hashes (while needed), the registration **outbox / reservation state** and the free-registration **queue**, attester authority material.
- **`username-read` owns:** the username mirror + search index (populated from finalized chain state).
- **Other services own** their own state if needed (for example, the invite/ticket pool); the notify relay remains stateless unless delivery requirements prove otherwise.
- Schema changes are generated migrations, applied per service; no cross-service schema ownership.
- **Current read/write boundary:** identity-service selects a discriminator through direct `UsernameOwnerOf` reads and does not depend on the username-read database. Username-read exists for discovery and prefix search, not registration correctness.

### APIs / Interfaces

Single public URL, path-namespaced. V1 ships **only endpoints a shipping iOS/Android build actually calls** — same paths, same payload shapes. Verified against shipping client source (iOS Swift + Android Kotlin):

- `POST /api/v1/auth/challenges` — attestation challenge (iOS + Android)
- `POST /api/v1/auth/app-attest/attestations` — verify App Attest, persist key (iOS)
- `POST /api/v1/auth/token` — SR25519 proof + attestation → `{token, refreshToken}` (iOS + Android)
- `POST /api/v1/auth/token/refresh` — rotate refresh token (iOS + Android). V2 serves this **clean** path; the prod double-mount `…/refresh/token/refresh` is a known V1 bug, **not reproduced** (verified against shipping iOS + Android).
- `GET  /api/v1/attester` — attester public key (iOS + Android)
- `POST /api/v1/usernames/available` — availability + digit suffixes (iOS + Android)
- `POST /api/v1/usernames` — register (async `202` → outbox → identity-chain-writer); `who` bound to the JWT subject (iOS + Android)
- `GET  /api/v1/usernames/search` — assigned-username search (iOS + Android)

**Also rebuilt in the workspace** (thin, so the old API can be fully retired): `/api/v1/notify`, `/api/v1/turn/issue`, `/api/v1/invitation-ticket/claim`. `/api/v1/subscriptions*` is **not built** — confirmed unused (the Android sync code is commented out; iOS never called it); deferred to notifications v3. **Also not built for V1** (no shipping caller): `GET /usernames/:username` status poll, deprecated `GET /usernames`, `POST /poc/issue` (no shipping client implements proof-of-compute — deferred to Future Work).

**JWT contract:** external services validate identity via JWT + JWKS only; they must **not** read identity-service tables.

### Technology Choices

- Rust, Tokio, **axum** (HTTP), `subxt 0.50` (People Chain + Asset Hub), Postgres (compile-time-verified queries + explicit transactions).
- **Asymmetric JWT + JWKS**, EdDSA/Ed25519, `identity-service` sole issuer, others verify-only via `auth-verify` — replacing V1's shared HS256 secret.
- **Play Integrity:** V1 uses Google's verify API (simple, fast); local self-managed-key verification is the deferred escape hatch (see Risks / Future Work).
- Observability: `tracing`, OpenTelemetry, Sentry (or approved equivalent).
- Quality gates: `cargo fmt`, `clippy -D warnings`, `cargo test`, `cargo-deny`, mutation testing (`cargo-mutants`).

### Security Considerations

- **Single JWT issuer, verify-only consumers** — asymmetric EdDSA/Ed25519; compromise of a sibling service cannot mint tokens (it holds only the public key).
- **Registration bound to the caller's account** — `POST /usernames` requires `who` (the beneficiary SS58) to equal the authenticated JWT subject (plus the account's own signature), so you can't register a username for an account you don't control. Gifting/referral would be an explicit future opt-in.
- **Rate limiting keyed on the JWT subject (SR25519), never on raw IP** — a whole venue shares one IP (the Summit problem). Layered: Cloudflare edge (primary) + per-JWT backend backstop. This is V1's anti-abuse protection on the unauthenticated handshake (a proof-of-compute gate is Future Work).
- **Per-service secrets and databases** — least blast radius; `identity-service` boots without any notification / TURN / invite secrets.
- **Least-privilege chain signing** — identity-chain-writer uses a Proxy of a cold attester authority, not the primary key.
- **Config/secret validation** — validate required env/secrets in CI **and at startup**; fail loud rather than half-start (see Risks: the 01/28 outage).

### Observability & Monitoring

- Per-service **readiness** (can take traffic) vs **liveness** (should restart); standard metrics/logs/traces via `tracing` + OpenTelemetry + Sentry.
- **Alert classes:** username-indexer lag/freshness; identity-chain-writer stuck / outbox age; free-registration & invite-pool exhaustion; Play Integrity quota nearing the daily cap; attester account balance; People Chain liveness; config/secret validation failure at startup.

### Risks

- **`subxt` gaps** — a spike may reveal missing People Lite / DotNS / indexing ops → narrow dynamic-call adapters behind SCALE-fixture tests.
- **Chain metadata drift & attestation-vendor changes** (Apple / Google).
- **Cutover parity** — a mismatch breaks shipping clients → contract tests against real client calls.
- **Gateway as shared front-door** — keep it thin (routing only), independently scalable.
- **Write-throughput ceiling** (V1's one active writer) → proxy sharding / batching (see Alternatives).
- **Deploy/config outage** — the 01/28 incident: an env-var edit on `main` auto-synced and crash-looped all pods for 8 days → validate required env/secrets in CI + at startup, fail loud, gate prod-config changes.
- **Free-registration / invite-pool exhaustion** — the pool ran dry at Summit, blocking registration → monitoring + on-chain invites (Future Work).
- **Multi-replica races / nonce collisions** — two writers sharing a signing account collide → one active writer via Postgres lease; never share the signing account.
- **Play Integrity 10k/day quota** — V1 uses Google's verify API. Mitigated now by **rate limiting + monitoring**; the escape hatches are a **proof-of-compute handshake gate** and **local self-managed-key verification** (both Future Work — the latter removes the cap entirely). Monitor headroom and migrate before it bites.

## Alternatives considered

- **Separate repos / separate owners per service** (the original proposal's framing) — rejected: adds people and cross-owner coupling; the W3S lesson is that this is fragile. A single workspace with isolated deploys gives the same isolation without the ownership sprawl.
- **Migrate only `identity-service`, leave the other services on existing infra** — not chosen: it keeps the legacy TypeScript backend alive, so every future change means patching two stacks and V1 can never be fully decommissioned. (JWT is **not** the blocker — a shared or public key validates tokens across infra just fine.)
- **JWT signing — shared secret (HS256) vs. asymmetric (EdDSA/Ed25519 + JWKS).** Chose **asymmetric, verify-only**: `identity-service` signs with a private key, siblings verify with the public key and **cannot forge**. The shared-secret model (V1's) would put the signing key in every service and force secret distribution/rotation — breaking the verify-only isolation this design depends on. Invisible to the app (the client treats the JWT as an opaque bearer token), so it stays 100% compatible.
- **TURN — thin cred service vs. client-minted.** V1 keeps a thin `/turn/issue` service: it preserves 100% compatibility (no Nova app change) and keeps the shared secret server-side. Client-minted creds (coturn shared-secret) would need the secret on the client — insecure if embedded, or still a backend call to deliver it — and a client change. Decentralizing TURN is Future Work.
- **Write throughput — one writer vs. many.** V1 starts with one active writer (one nonce lane) for simplicity. Alternatives for scale: **proxy accounts** (e.g. 10 proxies each batching ~50 registrations), `utility.batch`, and **multi-nonce** concurrent submission. Adopt when registration volume demands it.
- **Exact availability — backend endpoint (V1) vs. on-device (future).** V1 serves the backend `/usernames/available` (no app change → 100% compatible). Moving the check on-device (`UsernameOwnerOf`) and the discriminator on-chain — which would remove the backend index dependency — is Future Work.
- **Single-URL gateway vs. per-service subdomains** — chose the single URL to preserve API compatibility and avoid client changes.
- **Shared vs. per-service database** — chose per-service DBs for isolation (exact granularity is an Open Question).

## Open Questions

Two decisions must be resolved to ship V1:

- **DB split granularity** — one shared Postgres instance with a database per service, fully separate instances, separate schemas, or archival-then-delete of legacy tables? Trades isolation against operational cost.
- **Cutover & data migration** (the biggest open decision) — do we **migrate V1 data or wipe and start fresh**? How does each relocated route switch over without downtime, and when is the old backend fully retired? The goal is a clean path that **fully decommissions V1**, not two backends running indefinitely.

## Future Work — Beyond V1

Deferred beyond V1 (some still need research / a feasibility check). An Open Question graduates here once answered. Framing (Torsten): **"move to chain what can move."**

- **Proof-of-compute handshake gate** — a stateless anti-abuse throttle for the unauthenticated login handshake (challenge / attestation / token) to protect the expensive attestation path + the Play Integrity quota. Deferred because **no shipping client implements it** (needs a client change); it is an anti-abuse throttle, **not** authentication (that's the JWT).
- **Notifications v3** — a dedicated notifications service (a v3 document already exists); revive the subscription surface (`/subscriptions`, currently commented out in the app) and rules; potentially client-side chain subscriptions.
- **Writer scaling** — grow the identity-chain-writer via proxy-account sharding + batching (`utility.batch`, multi-nonce) when registration volume demands it.
- **Username availability + discriminator → on-chain** (needs feasibility check) — the chain answers availability directly (`UsernameOwnerOf`, checkable on-device) and assigns the suffix on register. This moves the username read-model off the backend **and** removes the write path's local-index dependency. (App change required — hence Future, not V1.)
- **Invites → on-chain invite graph** (needs research) — each personhood grants N invites directly on-chain, dropping the custodial key pool that ran dry at Summit. `sign_up_with_invite` already runs on-chain and is free to the invited user.
- **Decentralized connectivity / TURN** (research) — signaling is already decentralized via the Statement Store; the media relay is the remaining centralized piece. Explore a host-managed WireGuard/Tailscale-style mesh, relay capability on infra people already run, or a peer-to-peer relay mesh.
- **Android without Google Play services (vanilla devices)** — hardware-keystore cert-chain attestation (`/api/v1/auth/android/attestation`); a spec already exists.
- **Play Integrity self-managed keys** — verify Play Integrity **Classic** tokens locally with Play Console keys (AES decrypt + EC verify), removing Google's 10k/day cap entirely. Verified officially supported; keep Google-server verification as a rotation fallback. (Precondition: the app uses Classic requests.)
- **Backend-less on-chain registration (speculative — may not be viable).** The app submits the attestation payload on-chain; the user logs in immediately with an auto-generated username and a **limited / not-yet-validated** status, validated by off-chain **oracles / controllers** listening to the chain. **Blockers:** the chain cannot verify Apple/Google attestation by itself (would need a custom node extension holding authority credentials), and limited-status accounts invite **spam**. Needs research into what verifying a username + device validity on-chain actually requires.
