> Stage: **abandoned — the service was removed (2026-08-17)**. This slice was built and wired, but
> never had a client: the 2026-07-22 correction below established that neither shipping app has ever
> called `/api/v1/dim-ticket`, and both obtain Game / ProofOfInk credentials through
> `POST /api/v1/invitation-ticket/claim`, which `invite-tickets` owns. Rather than keep two workloads,
> a Postgres, an inviter signing key and a nonce lane alive for a route with no consumer, the
> `dim-tickets` crate, both roles (`dim-tickets-api`, `dim-tickets-writer`), its database, its edge
> route and its OpenAPI surface were deleted, taking the standard topology from ten workloads to
> eight.
>
> **The route is now out of V2 scope**, recorded in `docs/architecture.md` under "Remaining scope &
> legacy retirement": it is not a required route, so V1 shutdown removes it rather than handing it to
> a V2 owner. `/api/v1/dim-ticket*` resolves to `identity-api` as the edge catch-all and answers its
> frozen 404.
>
> **Operator cleanup is out of band:** drop the external `dim-tickets` database and the local
> `dim_tickets_pgdata` volume, and retire the `DIM_DATABASE_URL` / `DIM_INVITER_SIGNER_SURI` vault
> entries. The DIM inviter account's remaining `AvailableInvites` are untouched by the removal.
>
> Everything below is the historical record of the slice as built. Do not treat it as current design.

# DIM tickets — game / personhood invitation-credential service

> **Correction (2026-07-22)**: this plan originally assumed `/api/v1/dim-ticket` was the shipping
> mobile route. Checking the client sources (iOS `InvitationApi.swift`, Android
> `InvitationTicketNetworkApi.kt`, and `git log --all -S 'dim-ticket'` over both repos) shows
> neither app has **ever** called it: both apps obtain Game / ProofOfInk credentials via
> `POST /api/v1/invitation-ticket/claim` `{who, dim}` (synchronous signature response, no status
> polling). The legacy backend serves both routes side by side, but `/api/v1/dim-ticket` (legacy
> PR #1969) has no known consumer; the apps integrated against the later claim flow (legacy PR
> #2216+). The route parity built here is still exact vs legacy, but **app compatibility is
> delivered by the `invites` claim slice**, which should be sequenced first.

# Goals
- Preserve the legacy `/api/v1/dim-ticket` behavior (request + status of Game / ProofOfInk
  invitation credentials) in an independently deployable Rust service.
- Reach finalized People Chain state (`Game.set_invite_ticket` / `ProofOfInk.set_invite_ticket`)
  through a durable DB-backed state machine and a **single** chain-submitting worker, mirroring the
  identity-service outbox/writer pattern.
- Isolate the inviter signing key and game-chain failures from login, attestation, and username
  registration.

# Non-goals
- Invitation tickets (`/api/v1/invitation-ticket/claim`) — that is the separate `invites` slice
  (backend-generated keypair pools). This service is DIM tickets only: user-submitted SS58, backend
  pays gas, JWT-gated.
- Any change to the legacy wire contract — paths, payloads, status codes, and quirks are frozen.
- Per-user quotas beyond the on-chain `AvailableInvites` check (legacy has none).
- Gateway implementation, data cutover execution, and legacy shutdown (separate plans).

# Design principles
- User journey drives order.
- Verify-only JWT: validate identity-service's asymmetric JWT via JWKS material; never mint tokens,
  never call identity-service synchronously, never read its tables.
- Per-service persistence: this service owns the `dim_tickets` table in its **own** Postgres. The DB
  row is source of truth; chain is reconciled to it.
- One inviter signer = one nonce lane = one writer instance. Same invariant as
  `identity-chain-writer`; never scale the writer.
- `chain-types` is the only place chain types live — extend its vendored metadata; transport and
  signing stay in this service.

# User journey
(As designed for this route; per the correction above, no shipping app implements this journey —
the apps use the synchronous claim flow instead.)
1. A client holding an identity-service JWT POSTs `/api/v1/dim-ticket` with the user's
   SS58 address and the DIM (`Game` or `ProofOfInk`).
2. The service checks the inviter's on-chain `AvailableInvites` for that DIM; if none remain the
   client gets the legacy `422 { "error": "No available invites" }`.
3. Otherwise a `PENDING` row is inserted (200 with the frozen create response) and the client polls
   `GET /api/v1/dim-ticket/{who}`.
4. The background writer batches pending tickets into `Utility.force_batch` of
   `set_invite_ticket` calls, signs as the inviter (optionally as `Proxy.proxy` of a cold primary),
   and advances rows to `REGISTERED` (with `onchainData`) or retries with backoff up to the cap,
   then `FAILED`.
5. The client sees `REGISTERED` and the game/personhood flow continues on-chain.

# Foundations / Already shipped (✅)

## People Chain typed surface and writer pattern
- What exists: `chain-types` vendors trimmed People Chain metadata and already implements the full
  People transaction-extension pipeline — including the no-op encodings for `GameAsInvited` and
  `AsProofOfInkParticipant` (`&[0]`) that the legacy TS signer hand-patched as
  `VerifyMultiSignature`-style extensions. `identity-service` has the working chain client,
  signer/proxy submission (derived from `ATTESTER_ACCOUNT`), outbox worker, tower-http stack, fail-fast
  config, and auto-run sqlx migrations to copy from.
- ✅ Demo: `identity-chain-writer` submits and finalizes People Chain calls on Paseo Next v2 today.
- Gaps: the vendored metadata is trimmed to
  `System,Balances,Utility,Proxy,People,PeopleLite,Resources` — **no `Game` / `ProofOfInk`
  pallets**; a metadata refresh including both is required first.

## JWT issuer and auth contract
- What exists: identity-service publishes Ed25519 JWKS at `/.well-known/jwks.json`; clients
  authenticate with `Authorization: Bearer <JWT>`. (The mobile clients use this JWT scheme
  against `/api/v1/invitation-ticket/claim`, not `/api/v1/dim-ticket` — see the correction at
  the top.)
- ✅ Demo: Identity Service MVP issues verifiable JWTs; wire-contract tests lock the convention.
- The shared `crates/jwt-verify` crate exists (extracted 2026-07-16); identity-service delegates
  its own verification to it, and notify-relay consumes it too. This service gates its routes on
  it in Phase 1.

## Legacy reference (ground truth for parity)
- What exists: `paritytech/identity-backend`, under
  `apps/identity-backend/src/routes/v1/dim-ticket.*` (routes + response shapes),
  `src/features/dim/` (shell, daemon shell, blockchain adapter, inviter signer),
  `src/supervision/dim-ticket/` (poll worker: 6s interval, batch 10, max 5 retries, exponential
  backoff 1s→60s), and `packages/dim-ticket` (state machine + batch planning pure core).
- ✅ Demo: the legacy backend serves the DIM ticket routes in production. (Correction 2026-07-22:
  no known client calls them — the apps claim DIM credentials via
  `/api/v1/invitation-ticket/claim`; see the note at the top.)
- Gaps: the inviter secret and game-chain faults share the legacy monolith's failure domain, and
  the daemon competes for the monolith's single `daemon-leader` lock.

# MVP phases (ship-shaped, demoable)

## Phase 0: Chain surface — DONE 2026-07-16
- **Goal**: the workspace can type-check Game/ProofOfInk calls and queries.
- **Scope checklist**:
  - [x] Refresh `crates/chain-types/metadata/people.scale` adding `Game` and `ProofOfInk` to the
        trimmed pallet list; update the provenance header and refresh command in `lib.rs`.
  - [x] Add smoke tests proving `Game.set_invite_ticket` / `ProofOfInk.set_invite_ticket` calls and
        `AvailableInvites` storage lookups construct (same style as the existing
        `builds_register_lite_person_call` test).
  - [x] Confirm identity-service and username-indexer still build against the refreshed metadata.
- **✅ Demo (passed 2026-07-16)**: full gate (fmt + clippy `-D warnings` + workspace tests) green
  with the new pallets typed; runtime unchanged (`next-people-paseo` specVersion 1000029).
- **Risks / failure modes**:
  - Metadata refresh bumps spec/transaction versions for the whole workspace — coordinate with any
    in-flight identity-service work.

## Phase 1: Compatible HTTP boundary (own service, own Postgres)
- **Goal**: accept and answer the two frozen routes behind verify-only JWT, with the create path
  quota-checked against live chain state — no chain submission yet.
- **Scope checklist**:
  - [x] New service crate `dim-tickets` (2026-07-16) with bins `dim-tickets-api` and
        `dim-tickets-writer` (writer = topology skeleton: own process/config/DB, logs the due
        backlog; submission is Phase 2); liveness/readiness on the shared tower-http pattern
        (`/readyz` gates on Postgres **and** the People RPC — the create path needs the chain).
  - [x] Own Postgres + migration creating `dim_tickets`:
        `ticket` (SS58, **primary key** — one ticket per address, across DIMs, forever),
        `inviter`, `network` (`westend2|paseo|polkadot`), `dim` (`Game|ProofOfInk`),
        `status` (`PENDING|SUBMITTING|SUBMITTED|REGISTERED|FAILED`), `onchain_data` jsonb,
        `retry_at`, `retry_count`, `created_at`, `updated_at`, plus the legacy indexes.
        Delta vs plan-as-drafted: the legacy `registered` bool is **derived** on the wire
        (`status = REGISTERED`), not stored — legacy itself had to backfill one from the other.
  - [x] Extract `crates/jwt-verify` (JWKS/public-key loading + Ed25519 verification + claims
        validation) — done 2026-07-16; identity-service now delegates its own verification to it
        and its `siblings_verify_issued_tokens_via_jwks` test pins the issuer↔verifier contract.
        Both dim-ticket routes gate on it (the legacy middleware's three 401 problem-details
        variants are reproduced).
  - [x] `POST /api/v1/dim-ticket` `{who, dim}`: reject `who == inviter` (server error, as legacy
        `die`s); read `Game|ProofOfInk.AvailableInvites(inviter)` with a bounded timeout (legacy:
        15s); `<= 0` → `422 {"error":"No available invites"}`; insert-or-conflict → conflict is
        `409 {"error":"Ticket already exists"}`; success is **200** (not 201/202) with the frozen
        create response (`ticket` and `who` both echo the user address, `status: "PENDING"`,
        `registered: false`, ISO timestamps). Legacy quirk preserved: an SS58-invalid `who` that
        passes the non-empty-string body check is a **500**, not a 400.
  - [x] `GET /api/v1/dim-ticket/{who}`: frozen status response; DB `SUBMITTING` **maps to
        `SUBMITTED`** on the wire; `registered` kept as the deprecated bool; `onchainData`
        nullable; missing row → `404 {"error":"Ticket not found"}`; no SS58 validation (legacy).
  - [x] Rate-limit by authenticated JWT subject (legacy `authActions` bucket), no raw-IP keying;
        429 problem details + `Retry-After`.
  - [x] Fail-fast config: `DATABASE_URL`, `PEOPLE_RPC_URL`, `PEOPLE_NETWORK`, `INVITER_ADDRESS`, and
        JWT key material (`JWT_JWKS_JSON` or `JWT_ED25519_PUBLIC_KEY`) required with no defaults;
        timeout, rate-limit, rate-window, and writer-poll values must be positive.
  - [x] (2026-07-22) Golden legacy fixtures: 29 request/response pairs captured from the REAL
        legacy route (commit `c8369d9`; harness + provenance in `tests/fixtures/`), replayed in
        `tests/legacy_wire.rs` — 19 through the production router offline (dead lazy PG pool +
        mock-RPC chain client), 10 against the response builders/renderers. The capture corrected
        four pre-capture approximations: 400 messages are zod v4 (not the guessed v3 texts), the
        500 body is the global hook's `Oops! Something went wrong.` (was `Internal Server Error`),
        malformed JSON is the Hono plain-text 400 + missing JSON content-type is a 500 (was a
        single problem-details 400), and unknown paths/methods are the Hono text `404 Not Found`
        (was axum's default 404/405).
  - [x] (2026-07-22) `#[utoipa::path]` on both handlers + doc-only schemas; crate registered in
        `apidoc-gen`; API reference regenerated (dim routes now documented).
- **✅ Demo (partial, 2026-07-16)**: against a local Postgres + live Paseo Next v2 RPC with a real
  EdDSA JWT: 401×3 problem-details variants, 404, 400 with field errors, live on-chain quota check
  → 422, SS58-quirk 500, and seeded-row GETs proving `SUBMITTING→SUBMITTED` + `registered` +
  `onchainData` mapping all verified over the wire. Remaining for the full demo: a funded inviter
  for the 200/409 create paths, and byte-diff against captured legacy fixtures.
- **Risks / failure modes**:
  - The quota check is a synchronous chain read on the request path — the route needs a timeout
    budget and a clear 5xx (not 422) when the RPC is down.
  - The `ticket` primary key spans DIMs: the same address cannot get a Game **and** a ProofOfInk
    ticket. That is legacy behavior; preserve it, don't "fix" it.
  - Frozen non-RFC-9457 error bodies (`{"error": "..."}`) on 404/409/422 — don't normalize them.

## Phase 2: Chain writer (single-instance registrar)
- **Goal**: pending tickets reach finalized on-chain registration with the legacy state machine.
- **Scope checklist**:
  - [x] `dim-tickets-writer` bin (2026-07-16): poll loop with the legacy tunables (6s interval,
        batch 10, max 5 retries, exponential backoff base 1s / cap 60s / exponent cap 10 —
        **un-jittered**, matching the legacy pure core; the jitter in the legacy worker applied
        only to its tick-error inner retries), single instance, `writer_lease` deploy-overlap
        guard with heartbeat renewal during finalization waits. Own `WriterConfig::from_env`
        (requires the API partition keys `PEOPLE_NETWORK` / `INVITER_ADDRESS`, validates the
        effective signer or proxy primary against the inviter, and needs `INVITER_SIGNER_SURI`;
        no JWT material).
  - [x] Fetch due rows: `status IN (PENDING, SUBMITTING, SUBMITTED)` and (`retry_at IS NULL` or
        `retry_at <= now`), scoped to the configured network and inviter, FIFO by
        `created_at`, limit = batch size; malformed rows logged and skipped. State mutations
        retain the same network/inviter scope.
  - [x] Batch planning as a pure decision (`plan.rs`, port of `planBatchProcessing`): retries
        exhausted (`retry_count >= max`) → `FAILED`; orphaned in-flight rows past their lease →
        recovered to `PENDING` **and re-batched the same tick** (legacy); batch marked
        `SUBMITTING` under one `retry_at` lease = `now + delay(1 + max retry_count in batch)`.
        Whole-batch submission errors leave rows `SUBMITTING` for lease-expiry orphan recovery
        and do NOT increment `retry_count` (legacy).
  - [x] Submit `Utility.force_batch` of per-DIM `set_invite_ticket` calls (static `chain-types`
        calls, chain-fetched nonce — one extrinsic in flight at a time); split ordered
        `Utility.ItemCompleted`/`ItemFailed` events positionally into `REGISTERED` (persist
        `onchainData` block hash/number; `updated_at` untouched — legacy quirk) vs `PENDING`
        with `retry_count + 1` and backoff `retry_at`. Zero item events → whole batch failed +
        warning; count mismatch → warning (legacy `parseForceBatchResults`).
  - [x] Submission/finalization timeout (default 120s, `WRITER_FINALIZE_SECS`) covers
        signing, nonce lookup, submission, finalization, and finalized-header lookup; structured
        logs per tick (registered/failed counts, block number, warnings). Metrics/counters are
        the production-hardening slice, as for the other services.
  - [x] Proxy signing derived from `INVITER_ADDRESS` vs the signer's own account (`INVITER_PROXY_FOR`
        removed in #46): batch wrapped in
        `Proxy.proxy(real, force_proxy_type: Some(Any))` — legacy signed with `Any`, unlike
        identity-chain-writer's `None`.
  - [x] Extracted `crates/chain-client` (reconnecting connect + `WriterSigner` + `proxy_target`) — dim-tickets was
        the second consumer; identity-service now consumes it too (its local copies deleted).
- **✅ Demo (partial, 2026-07-16)**: against local Postgres + live Paseo Next v2 with `//Alice` as
  inviter: a seeded `PENDING` ticket was batched, signed, submitted, and finalized on-chain five
  times (blocks 1751454, 1751472, 1751488, 1751509, 1751535); each `ItemFailed` (unfunded
  inviter) drove a real `PENDING → SUBMITTING → PENDING(retry_count+1, backoff)` cycle, and after
  the fifth failure the ticket was marked exhausted and landed terminally `FAILED` — the complete
  unhappy-path state machine observed end-to-end through real extrinsics. Remaining for the full
  demo: a **funded** inviter so items complete (`REGISTERED` + `onchainData` via the API).
- **Risks / failure modes**:
  - `force_batch` event-index parsing must match legacy exactly or success/failure flips per item.
  - The `retry_at` lease is the only orphan-recovery mechanism — a crashed writer must strand no
    row in `SUBMITTING` forever.
  - Never run two writers; the inviter nonce lane is the real serializer.

## Phase 3: Independent deployment readiness — DONE 2026-07-16
- **Goal**: safe to deploy, fail, and stop without touching any sibling service.
- **Scope checklist**:
  - [x] Compose/deploy entries (2026-07-16): `dim-tickets-postgres` (host :5434),
        `dim-tickets-api` (host :8082), `dim-tickets-writer` (`replicas: 1`), all reusing the one
        workspace image (Dockerfile builds the two new bins). Service-owned secrets: the dim
        services get explicit `environment:` blocks with **no `.env` pass-through** — the inviter
        SURI exists only in `dim-tickets-writer`'s env, the identity JWT signing seed never
        reaches a dim container (only the public key does).
  - [x] Failure isolation proven live (compose): with `dim-tickets-api` + writer + its Postgres
        all stopped, identity-api `/readyz` stayed `200 ready` and only :8082 refused; the dim
        stack came back ready **without identity-api restarting** (container `StartedAt`
        unchanged); with only the writer stopped, the read path kept serving (GET answered;
        rows just stay `PENDING` — proven in the Phase 2 smoke).
  - [x] Redaction: `WriterConfig` and `Config` implement `Debug` by hand — the inviter signing
        secret and the DB password render as `<redacted>`; pinned by
        `debug_output_redacts_signer_secret_and_db_password`. Startup logs print the signer's
        **public** key only.
  - [x] Caddy route documented in `docs/deploy.md` (`/api/v1/dim-ticket*` → 127.0.0.1:8082),
        plus topology + secret-boundary paragraphs, dim env vars in the `.env` template, verify
        curls, and troubleshooting rows. No cutover implemented.
  - [x] `docs/architecture.md` updated across the phases (service row **exists**; classification;
        `invites` row points here; data-flow invariants gained the DIM ticket state-machine +
        single-writer bullet) and `docs/roadmap.md` lists the slice as active.
- **✅ Demo (passed 2026-07-16)**: full drill against the compose stack — dim services stopped →
  only `/api/v1/dim-ticket*` down, identity-api untouched; restarted → a real identity-key JWT
  authenticated against dim-tickets-api and the frozen routes answered (404 / live-quota 422)
  with the writer still stopped.
- **Risks / failure modes**:
  - Gateway and cutover are separate plans; direct service testing suffices here.
  - Compose reads the shared `.env` for interpolation only; keep every service's explicit
    environment allowlist current when adding configuration so signing secrets never cross
    process boundaries.

# Contracts (guardrails)
- Frozen routes: `POST /api/v1/dim-ticket` and `GET /api/v1/dim-ticket/{who}`, exact field names,
  exact status codes (200 create, 400 RFC-9457 on body validation only, 401, 404/409/422 with the
  legacy `{"error": "..."}` bodies).
- Wire statuses are `PENDING|SUBMITTED|REGISTERED|FAILED`; internal `SUBMITTING` is never exposed.
- `Authorization: Bearer <JWT>` verified with public-key material only; username search-style
  public access does **not** apply here — both routes require auth.
- Own Postgres; never read identity-service or username-indexer tables; no service reads ours.
- Exactly one writer instance; the API bin never submits extrinsics.
- Chain types come from `chain-types` only; transport + signing live in this service.

# Key decisions
- **Inviter nonce lane vs invite-tickets-pool** (decided 2026-08-04, cross-slice with the
  invite-claim plan): the two chain-submitting bins use **distinct inviter accounts**. Compose
  interpolates this writer's signer from `INVITER_*` (dev default `//Alice`) and
  `invite-tickets-pool`'s from `INVITE_INVITER_*` (dev default `//Bob`), so they cannot coincide
  by default; `docs/deploy.md` ("Nonce lanes") mandates distinct accounts in production —
  e.g. separate proxy delegates of one cold inviter primary. Deploying this writer therefore
  needs its own funded inviter account, never the invite pool's.
- **Service name** (decided 2026-07-16): `dim-tickets`. Update architecture.md vocabulary when the
  crate is created.
- **Data migration** (decided 2026-07-16): start **fresh** — no legacy row copy in this slice. The
  door stays open: the table keeps the legacy column set and status/value encodings so a later
  one-time copy from the legacy `polkadot_app.dim_tickets` table is a plain `INSERT ... SELECT`.
  Revisit at cutover (architecture.md open decision 3). (Correction 2026-07-22: no known client
  polls the GET route — see the note at the top — so skipping the backfill would not 404 any
  shipping app; whether the legacy rows are worth retaining is a data question for cutover.)
- **DIM priority** (decided 2026-07-16): `Game` is the shipping priority. Parity fixtures cover
  both enum variants (frozen contract), but testnet/real-device smokes target Game first;
  ProofOfInk smoke is a fast-follow, not a phase gate.
- **`jwt-verify` extraction timing** (decided 2026-07-16): dim-tickets runs in parallel with the
  active notify-relay slice, so the "second consumer" condition is met at start of work — extract
  `crates/jwt-verify` **immediately as a micro-slice** (JWKS/public-key material loading + Ed25519
  verification + claims validation, nothing more) and both services consume it from day one.
  Note the rationale is one implementation of the cross-service auth contract, **not** failure
  isolation — the crate is statically linked per service; runtime independence comes from separate
  processes, own secrets/DBs, and verify-only JWKS (no synchronous identity-service call).
- **Writer lease shape**: reuse the `writer_lease` best-effort pattern vs rely purely on
  `retry_at` leases (legacy behavior). Default: legacy behavior + deploy-overlap lease.

# Testing
- Golden request/response fixtures captured from the legacy route tests for every status code.
- State-machine tests for batch planning: exhaustion, orphan recovery, mixed
  `ItemCompleted`/`ItemFailed` splits, backoff arithmetic at the exponent cap.
- Integration tests with real Postgres + fake chain client (identity-service test pattern):
  conflict on duplicate, quota-exhausted, RPC-down 5xx, writer tick end-to-end.
- Paseo Next v2 smoke: real registration for `Game` (priority) and proxy-signing mode; `ProofOfInk`
  smoke as fast-follow.

# Polish rounds (after MVP)

## Polish round 1: Throughput and observability
- Measure quota-check latency on the request path; consider a short-TTL cached read if measured
  load requires it (not before).
- Registration latency histograms, pool/backlog gauges, and alerts on `FAILED` growth and writer
  stall (no tick progress).
- ✅ Check-in demo: fault injection (RPC flaps, DB restarts) stays within agreed budgets without
  affecting another service.

# Later / Deferred
- Per-user issuance quotas (JWT-subject-keyed) — legacy has none; new product behavior needs its
  own decision.
- Gateway routing, release-device validation, data migration execution, and legacy shutdown live in
  the cutover plans.
