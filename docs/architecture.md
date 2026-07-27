# Identity Backend V2 — Architecture & Naming Alignment

> Current architecture, scope, service boundaries, naming, and data-flow invariants for humans and
> agents. Code and tests remain the source of truth; update this file in the same change whenever
> implementation changes make it inaccurate.

## Vision

Identity Backend V2 replaces the coupled TypeScript backend with **one Rust Cargo workspace of
small, independently-deployable services under one team**. Boundaries exist to isolate secrets,
persistence, deployment lifecycle, and failures: a notification, TURN, invite, or indexing fault
must never disrupt login, attestation, or username registration.

Principles that drive every boundary decision:

- **`identity-service` is the sole JWT issuer.** Siblings are *verify-only* — they validate its
  asymmetric JWT via JWKS **when a route needs auth**, but never mint tokens, never call
  identity-service synchronously, and never read its tables. *Verify-only does not mean every
  route requires a JWT* — e.g. username search is public.
- **Shared code is minimal and product-agnostic.** No shared database, no shared product logic.
- **Per-service persistence.** Each stateful service owns its data; stateless services add none
  without evidence. No shared tables.
- **The mobile API is frozen** — same base URL, same paths/payloads — behind one path-routing
  front door. V2 rebuilds every live route and **fully retires V1**; two permanent backends is a
  failure state.

## Naming convention

**Plain, boring, descriptive names. No decorative prefixes.**

- A name says what the thing **is/does** in plain English — a new engineer guesses the role in <1s.
- **No workspace prefix** (`id-`, `shared-`, `svc-`, `lib-`). The repo is already the namespace;
  `Cargo.toml` and `docker ps` show the plain name. The non-identity services (`notifications`,
  `turn`, `invites`) are *not* identity — don't imply otherwise.
- Lowercase kebab-case, short, greppable. Boring is good.
- **No themed/metaphorical names** (no `mint`/`citadel`/`signet`/nautical/etc.) and **no invented
  `-ity` brands** (the `-ity` policy is public-facing only and exempts crate/CLI names; also avoids
  colliding with Trinity/Levity/Nominality/…).

## Naming vocabulary

This is the **agreed vocabulary** for parts as they come to exist — **not** a list of folders to
create now. See "How the workspace grows" below.

### Shared library crates (product-agnostic, imported by services)

| Concept | Name | Status |
|---|---|---|
| generated People Chain (subxt) typed surface | `chain-types` | **exists** (shared by identity-service + username-indexer + dim-tickets + invite-tickets) |
| chain connection / RPC transport + writer signing key | `chain-client` | **exists** (crate: reconnecting connect + `WriterSigner`; extracted when dim-tickets became the second consumer; invite-tickets is the third) |
| Postgres connection + bootstrap | `postgres` | module in identity-service/username-indexer today |
| config loading + fail-fast validation | `config` | module today |
| telemetry (logs / metrics / tracing) | `telemetry` | not yet |
| JWT + JWKS verification (for verify-only services) | `jwt-verify` | **exists** (crate; identity-service delegates its own verification to it; dim-tickets, invite-tickets, and notifications consume it) |
| frozen legacy HTTP dialects + axum scaffolding (problem details / Hono text bodies / zod v4 messages, JWT extractor, rate limiter, health, middleware stack, env helpers) | `http-common` | **exists** (crate; extracted 2026-07-22 when invite-tickets became the second consumer of dim-tickets' scaffolding; dim-tickets, invite-tickets, turn, and notifications consume it — the dialect bodies are golden-captured, change only with a fresh capture) |

### Deployable services (plain domain word)

| Service | Responsibility | Status |
|---|---|---|
| `identity-service` | Device **attestation** + auth handshake + **sole JWT issuer** + username **write** path + **eligibility/queue** + reservation **outbox**. Bins: `identity-api` (N replicas) + `identity-chain-writer` (single-instance outbox→chain worker). | **exists** |
| `username-indexer` | Finalized-chain username **indexer + public reads**: prefix search (`GET /api/v1/usernames/search`, paginated, unauthenticated, per-IP rate limited). The deprecated single-username lookup (`GET /api/v1/usernames/{username}`) was removed 2026-07-23 in favour of `search` and now serves the Hono 404, like the sunset list endpoint. Read-only projection; can be down without affecting registration. | **exists** |
| `notifications` | Thin `/api/v1/notify` relay. Verify-only Ed25519 JWT, stateless, DB-free, per-subject rate limited. Independently deployable (own process/secrets, no `depends_on`). Bin: `notify-relay`. Real iOS APNs (token auth, HTTP/2) + Android FCM (v1, OAuth2) providers, each optional. | **exists** (live device smokes pending) |
| `turn` | JWT-gated `POST /api/v1/turn/issue` — stateless coturn REST-API credential issuer (username = expiry:id, password = HMAC over the username, secret shared with the relay; the relay itself = SRE infra). No DB, no chain; N replicas need no coordination. Bin: `turn-api`. Plan: `docs/plans/active/turn.md`. | **exists** |
| `invite-tickets` | JWT-gated `POST /api/v1/invitation-ticket/claim` — synchronous invitation-credential claims from a pre-staged, on-chain-registered sr25519 keypair pool (its own Postgres). This is the route the shipping apps call for Game / ProofOfInk DIM claims. Bins: `invite-tickets-api` (N replicas, DB-only, no signing secret) + `invite-tickets-pool` (single-instance keypair generator + on-chain registrar). Plan: `docs/plans/active/invite-claim.md`. | **exists** (pool refill demo pending a funded inviter) |
| `dim-tickets` | JWT-gated `/api/v1/dim-ticket` — DIM (Game / ProofOfInk) invitation-credential issuance + status. Own Postgres state machine; single-instance chain writer (`dim-tickets-writer`: `Utility.force_batch` of `set_invite_ticket`, per-item event split, retry/orphan recovery, `writer_lease`). No known client consumer — the apps use the `invite-tickets` claim route (see the plan's correction note). Plan: `docs/plans/active/dim-tickets.md`. | **exists** |
| gateway | Public-URL path routing via **Caddy** (reverse proxy). The route table is the committed `gateway/Caddyfile` (env-parameterized; used by prod's host Caddy AND the compose `gateway` service on :8000): migrated routes → their service (:8081–8085), unmigrated legacy routes → `LEGACY_BACKEND_ORIGIN` (route-by-route cutover lane), everything else → `identity-api` (:8080). Holds no secrets (enforced by the compose boundary script). Plan: `docs/plans/active/gateway.md`. | **exists** (committed artifact + compose service) |


## How the workspace grows (lazy extraction)

The vocabulary above is **not** "make 11 folders." Folders appear incrementally:

- **Today there are 11 crates:** `chain-types`, `chain-client`, `jwt-verify`, `http-common`,
  `identity-service`, `username-indexer`, `dim-tickets`, `invite-tickets`, `turn`,
  `notifications`, and `apidoc-gen`.
- **Services** become crates **when you build them** — a service gets a folder the day work
  starts, not before.
- **Shared libs are extracted lazily — only when a *second* service needs the code.** Until then
  the logic lives as **modules inside** the owning service (today: `identity-service/src/{config,db,chain}.rs`).
  `jwt-verify` was the first such extraction — pulled out the day `notifications` became the second
  verify consumer. Don't create a shared crate speculatively.
- **When you do extract, prefer a *few focused* crates, not extremes:**
  - ❌ 6 tiny crates scaffolded up front (premature boundaries, churn).
  - ❌ one `common`/`shared` grab-bag (every service recompiles + pulls everything).
  - ✅ a handful (realistically **3–4** over time) that each do one clear thing. `chain-types`,
    `jwt-verify` (the cross-service auth contract), and `chain-client` (reconnecting transport +
    writer signing key) are shared today — each extracted when dim-tickets became its second
    consumer; next candidate is optionally a small `platform` grouping of
    `config`+`telemetry`(+`postgres`) **only if** they always travel together.

## Classification quick-reference

- **attestation** → inside `identity-service` (NOT a service, NOT a shared lib).
- **availability** → `identity-service` (People Chain plus durable reservation-outbox read).
- **username write** → `identity-service`.
- **username read/search** → `username-indexer` — true carve-out.
- **free-reg eligibility/queue** → `identity-service` product logic.
- **chain-writer** → worker (`identity-chain-writer`) *inside* the `identity-service` boundary.
- **notifications** → independent thin service; **turn / invites** → future independent services.
- **DIM tickets (game/personhood)** → the `/api/v1/dim-ticket` routes live in the `dim-tickets`
  service — not `identity-service`. But the apps' game/personhood **claims** go through
  `/api/v1/invitation-ticket/claim`, which is the `invites` service.
- **gateway** → Caddy path routing (infra).

### Guardrails — do NOT create these as separate crates/services

`attestation`, `availability`, `eligibility`, `queue`, `payment`, `writer` all live **inside**
`identity-service`. Splitting any out contradicts the design (single identity failure/secret boundary).

## Current → target names

| On disk today | Target | Action |
|---|---|---|
| `chain-types` | `chain-types` | unchanged |
| `identity-service` (+ `identity-api`, `identity-chain-writer`) | unchanged | already correct |
| `username-read` → `username-indexer` | done | renamed |
| `notifications` | `notifications` | created; bin `notify-relay` |
| *(future)* turn / invites | `turn` / `invites` | name on creation |
| `jwt-verify` | `jwt-verify` | extracted (crate) |
| `chain-client` | `chain-client` | extracted (crate) |
| `http-common` | `http-common` | extracted (crate) |
| *(future)* shared libs | `postgres` / `config` / `telemetry` | extract lazily, per above |
| gateway | Caddy | routes username GET reads → :8081, DIM tickets → :8082, invitation tickets → :8083, TURN → :8084, notify → :8085, and identity-owned writes/availability/root preflight → :8080; unmigrated routes use the legacy cutover lane |

## Open decisions (need the user)

1. **Queue worker topology** — keep the queue-advancer as a separate daemon in V1 or consolidate
   with the writer later. Does not change ownership.
2. **Database topology** — separate instances vs databases vs schemas. ("No shared tables" already decided.)
3. **Migration/cutover** — migrate V1 data or start fresh, then route-by-route cutover + V1 shutdown.

## Remaining scope & legacy retirement

Not-yet-started slices. Each gets its own ship-first plan under `docs/plans/` when work begins:

1. iOS App Attest and DeviceCheck verification.
2. Android Play Integrity and TEE verification.
3. Registration eligibility, queue, payment, and voucher behavior.
4. Production hardening: real batching if measurements require it, load tests, metrics, alerts, and parity benchmarks.
5. iOS and Android release-device validation.
6. Data migration, rollback, route-by-route cutover, and legacy shutdown.

The legacy backend is retired only after every required route has an owner, the gateway is validated, parity gates pass, rollback is proven, and release-device smokes succeed.

## Follow-up

- (done) `AGENTS.md` now lists all three crates including `username-indexer`.

## Implementation notes (data flow & invariants)

Operational invariants an agent must respect when touching the code.

- **Registration is an outbox.** `POST /api/v1/usernames` inserts a `RESERVED` row in
  `username_reservations` and returns `202`; `identity-chain-writer` claims rows and submits the
  People Chain calls (`PeopleLite.attest` / `Resources.register_lite_person`), advancing
  `RESERVED → SUBMITTING → ASSIGNED | RETRY_AFTER | FAILED_TERMINAL`. The DB row is source of truth;
  chain is reconciled to it. No service reads another service's tables.
- **Registration is authorized by `candidateSignature`, not the JWT subject.** `POST
  /api/v1/usernames` verifies the beneficiary's sr25519 signature over `pop:people-lite:register
  using || candidatePubkey || ringVrfKey` (the message the chain reconstructs for `PeopleLite.attest`;
  Substrate signing context) before enqueuing. The JWT authenticates only the device/session — a
  separate per-install auth wallet — so `candidateAccountId` is deliberately **not** required to
  equal the JWT subject. Stored `account_id` (JWT subject) is the abuse-control/rate-limit key;
  `candidate_account_id` is the on-chain beneficiary the writer submits for.
- **`identity-chain-writer` is single-instance by design** — one signer = one nonce lane. The
  `writer_lease` table is a best-effort deploy-overlap guard only; the chain nonce + outbox
  reconciliation is the real serializer. Never run two writers / never scale it.
- **Signing:** the writer signs as a least-privilege `Proxy` of a cold attester primary when
  `CHAIN_WRITER_PROXY_FOR` is set, else directly as its SURI.
- **`chain-types` is the only place chain types live.** Static codegen from vendored metadata at
  `crates/chain-types/metadata/people.scale`; online transport + signing live in
  `identity-service::chain`, never in `chain-types`. Regenerate with the `subxt metadata …` command
  at the top of `crates/chain-types/src/lib.rs`.
- **HTTP** (`identity-service::http`): router in `http.rs` wrapped in a tower-http stack
  (request-id → trace → 30s timeout) with graceful shutdown. Health at `/livez`, `/readyz` (gates on
  a live Postgres `SELECT 1`), `/healthcheck`; JWKS at `/.well-known/jwks.json`. `AppState` =
  Postgres pool + `ChainClient` + `Jwt` + `Config`. `username-indexer` uses the same tower-http
  stack, but its `/readyz` also reports index freshness and it has no JWT/JWKS.
- **invite-tickets: the pool has two states and the claim is one transaction.** A ticket row is
  `available` (generated, registered on-chain) or `claimed` — nothing else; failed registrations
  are never inserted. The claim is: pre-check count (`0` → 422 `Pool exhausted`) → one transaction
  `SELECT … FOR UPDATE SKIP LOCKED ORDER BY created_at LIMIT 1` + flip to `claimed` (no row →
  409 `Ticket race lost`) → sign → post-claim count as `remaining`. The signature is sr25519 by the
  **ticket** key over the **decoded 32-byte account id** of `who` (substrate signing context), not
  the address string. Pools are keyed `(dim, network)`; FIFO by `created_at`. The api bin never
  touches the People Chain and holds no signing secret besides the pool rows it serves — chain
  RPC and the inviter secret belong only to the single-instance `invite-tickets-pool` bin.
  **`invite-tickets-pool` and `dim-tickets-writer` must not sign as the same account** — two
  independent submitters on one account race nonces; give each its own inviter (e.g. separate
  proxy delegates of one cold primary) before deploying both.
- **DIM tickets are a per-row state machine, not an outbox copy.** `POST /api/v1/dim-ticket`
  quota-checks the inviter's on-chain `AvailableInvites` synchronously, then inserts a `PENDING`
  row (200); `dim-tickets-writer` batches due rows into one `Utility.force_batch` of
  `set_invite_ticket` calls and splits the ordered `ItemCompleted`/`ItemFailed` events into
  `REGISTERED` vs `PENDING` retries (backoff, cap → `FAILED`). The `retry_at` written at
  `SUBMITTING` doubles as the in-flight lease: a crashed writer's rows come back as orphans and
  are re-batched. `dim-tickets-writer` is **single-instance** (one inviter key = one nonce lane;
  `writer_lease` is the deploy-overlap guard) — never scale it. The internal `SUBMITTING` status
  is reported as `SUBMITTED` on the frozen wire.
- **`username-indexer` serializes projection writes on one Postgres advisory lock.** Bootstrap and
  the incremental sync loop share one lock id; a full snapshot bootstrap runs **only when the
  `sync_state` checkpoint is empty**, and each sync pass takes the lock with `pg_try_advisory_lock`
  (skips the pass if another instance holds it). So the service is safe as N replicas on one shared
  Postgres, and restarts resume incrementally from the checkpoint (no full re-scan).
- **`username-indexer` public reads share one per-IP rate-limit bucket.** Search and single-username
  lookup are deliberately JWT-free; the gateway forwards both GET surfaces to the indexer, while
  identity-owned POST/OPTIONS traffic falls through to `identity-api`.
- **`notifications` is an isolated failure domain.** It is stateless, DB-free, and shares no
  `depends_on` with any other service, so `/readyz` needs no external probe (readiness = the bound
  listener; provider outages are per-request `200 success:false`, never unreadiness) and stopping it
  or feeding it bad provider secrets can only fail `/api/v1/notify`. Its `/api/v1/notify` route is
  rate-limited **per authenticated subject** (JWT `accountId`) keyed on `route:subject` — never raw
  client IP — enforced after JWT verification, `429 RATE_LIMITED` on exceed. It holds only the JWT
  **public** key (verify-only); push secrets (`APNS_*`/`FCM_*`) are service-owned.

## External references

- **Canonical username-registration spec** (ground-truth for the `/usernames` write path + future
  eligibility): Mike's "New JWT / Integrity / PoUD / Username Claim Logic" —
  https://docs.google.com/document/d/1YQy0how-ItgThH5VfC1SR0yR6j_Xh9jLTOn35Oo9ubc — covers Android
  TEE attestation (`POST /auth/android/attestation`), PoUD (Android `{androidId, widevineId}` / iOS
  DeviceCheck), the INSTANT / PAYMENT_REQUIRED / QUEUED decision flow, the balance-priority queue
  (G1<10 … G4≥1000), QR-voucher bypass, and `/usernames/payment-status` + `/registration/queue`.
  Digest it fully before implementing the attestation/eligibility plan.
