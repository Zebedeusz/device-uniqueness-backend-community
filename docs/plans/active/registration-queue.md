# Registration queue — balance-priority FIFO for username claims

> Stage: active. Keep this plan current while working: when a scope item is
> finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the
> phase done (with date). The plan file is the source of truth, not memory.

Implements the **Registration queue** portion of Mike's "New JWT / Integrity /
PoUD / Username Claim Logic" spec (the canonical username-registration spec
linked from `docs/architecture.md`): PoUD-passing devices enter a fair FIFO
queue with priority tiers based on account top-up; the backend registers
queued usernames on-chain in batches every N seconds.

# Goals

- A claim (`POST /api/v1/usernames`) can enter a durable registration queue
  instead of going straight to the chain-writer, reporting
  `registrationOutcome: "QUEUED"` plus queue data.
- Queued claims are assigned a **priority group from the claimant's on-chain
  balance** (G1 `<10` DOT, G2 `≥10`, G3 `≥100`, G4 `≥1000`), refreshed while
  they wait so a top-up moves them up.
- A **queue advancer** promotes up to 4 claims per iteration, every N seconds
  (server config), with the spec's slot eligibility (slot 1: G4; slot 2:
  G3+G4; slot 3: G2–G4; slot 4: all). Each slot picks the earliest-enqueued
  eligible claim; empty eligible set skips the slot. Promoted claims flow
  through the existing outbox → chain-writer path unchanged.
- **`GET /api/v1/registration/queue`** (NEW, JWT): returns
  `{queuePosition, group, estimatedIterationsRemaining}` for the caller's
  queued claim; `404` when there is none.

# Non-goals

- TEE attestation, PoUD checks, `PAYMENT_REQUIRED` / `INSTANT` outcomes, QR
  vouchers, and `GET /api/v1/usernames/payment-status` — those are the
  attestation/eligibility slice (`docs/architecture.md` remaining-scope items
  1–3). Until it lands, *every* claim on the queue-enabled path is the spec's
  default "PoUD pass → QUEUED" lane.
- Reshaping the claim response to the spec's `200 {registrationOutcome}`
  envelope. The shipping apps parse today's frozen `202` body
  (`tests/fixtures/legacy-wire.json`); the queue fields are **additive** on
  the 202 and the envelope change ships with the attestation slice + client
  coordination.
- Push/webhook notification of "your username is ready" — clients poll.

# Design principles

- User journey drives order.
- **The outbox row remains the single source of truth.** The queue is a new
  *entry state* (`QUEUED`) ahead of `RESERVED` in the existing
  `username_reservations` state machine — no parallel queue table, no second
  writer path.
- **Fail open on balance reads**: a chain hiccup while grouping must never
  block a claim — it degrades to group 1 (lowest priority), never to an error.
- Feature-gated: `QUEUE_ENABLED=false` (default) keeps the current M0 wire
  byte-identical.

# User journey

1. User claims a username; the app gets `202` with
   `registrationOutcome: "QUEUED"` and queue data.
2. The app polls `GET /api/v1/registration/queue` and shows position/ETA.
3. (Optional) User tops up their account; next advancer iteration their group
   rises and their ETA drops.
4. The advancer promotes the claim; the chain-writer registers it on-chain;
   the queue endpoint starts returning `404` (no longer queued) and the
   username appears on-chain / in the indexer.

# Foundations / Already shipped (✅)

## Reservation outbox + chain-writer
- What exists: `POST /api/v1/usernames` inserts a `RESERVED` row
  (`usernames/register.rs`, `chain/outbox.rs`); single-instance
  `identity-chain-writer` claims due rows and submits `PeopleLite.attest`
  (`chain/writer.rs`).
- ✅ Demo: compose stack registers a username end-to-end.
- Gaps: no waiting state — every valid claim is immediately writer-eligible.

## Chain client + auth
- What exists: `ChainClient` (People Chain reads; `System`/`Balances` pallets
  are in the vendored metadata), `AuthSubject` JWT extractor, subject rate
  limit, `Config::from_env` fail-fast config.
- ✅ Demo: `/available` reads chain through it today.
- Gaps: no balance read helper (added in Phase 1).

# MVP phases (ship-shaped, demoable)

## Phase 1: queued intake — claims can wait ✅ 2026-07-27
- **Goal**: a claim lands in the queue with a balance-derived group and the
  client is told so.
- **Scope checklist**:
  - [x] Migration `0005_registration_queue.sql`: `queue_group` column +
        partial FIFO index on `status = 'QUEUED'`.
  - [x] `Status::Queued` entry state + `outbox::insert_queued`.
  - [x] `ChainClient::free_balance` (`System.Account` → `data.free`).
  - [x] `queue` module: group thresholds (`group_for_balance`), drain
        simulation (`drain_estimate`), snapshot query.
  - [x] `QUEUE_ENABLED` config (default `false`); queued intake lane in
        `register` with additive `registrationOutcome` + `queue` fields
        (absent when disabled — frozen wire untouched, replay suite green).
- **✅ Demo**: with `QUEUE_ENABLED=true`, a claim returns
  `"registrationOutcome":"QUEUED"` + queue data and the row sits in
  `QUEUED`; with the flag off, the body is byte-identical to the capture.
- **Risks / failure modes**: balance read latency on the claim path
  (mitigated: single storage read, fail-open to group 1).

## Phase 2: queue advancer — the queue drains fairly ✅ 2026-07-27
- **Goal**: queued claims reach the chain in spec order.
- **Scope checklist**:
  - [x] Per-iteration group refresh from on-chain balances (top-up promotion).
  - [x] `advance_iteration`: 4 slots, tiered eligibility,
        earliest-enqueued pick, `FOR UPDATE SKIP LOCKED` promotion to
        `RESERVED`.
  - [x] Advancer loop wiring (`QUEUE_ADVANCE_INTERVAL_SECS`, default 6);
        compose + `.env.example`. *(Initially hosted inside the
        `identity-chain-writer` process; superseded by Phase 4's standalone
        service.)*
- **✅ Demo**: seed queued rows across groups; watch the writer log promote
  4 per iteration in slot order and the usernames land on-chain
  (live-Postgres test `queue_live` pins the slot algebra).
- **Risks / failure modes**: advancer racing a second instance (bounded:
  SKIP LOCKED keeps promotion exactly-once; the writer is single-instance by
  deployment contract anyway).

## Phase 3: queue status endpoint — the wait is visible ✅ 2026-07-27
- **Goal**: clients can render position/ETA.
- **Scope checklist**:
  - [x] `GET /api/v1/registration/queue` (JWT via `AuthSubject`): drain
        simulation over the live snapshot →
        `{queuePosition, group, estimatedIterationsRemaining}`; `404` when
        the subject has no queued claim.
  - [x] OpenAPI annotations + regenerated `docs/api-reference` artifacts.
  - [x] Gateway: no route change needed (`/api/v1/registration/*` falls
        through to identity-api) — verified against `gateway/Caddyfile`.
- **✅ Demo**: queued subject polls the endpoint and sees position drop each
  iteration; unqueued subject gets `404`.
- **Risks / failure modes**: snapshot simulation cost on hot polling — O(queue
  length), integers only; revisit only if measurements demand (per the
  "measured optimization waits" principle).

## Phase 4: queue as an isolated service — a dead queue can't brick claims ✅ 2026-07-27
- **Goal**: the queue is its own runtime service; unavailable, the backend
  falls back to the original direct registration behavior (user decision).
- **Scope checklist**:
  - [x] Standalone `registration-queue` bin/compose service (third
        identity-service binary; Dockerfile + compose + `.env.example`).
  - [x] Shared lease helpers (`chain/lease.rs`); the advancer holds the
        `registration-queue-advancer` lease — single-instance guard **and**
        liveness signal (`QUEUE_LEASE_TTL_SECS`, default 30, must exceed the
        iteration interval).
  - [x] **Intake fallback**: `identity-api` queues a claim only while the
        advancer lease is fresh; disabled, dead, or probe-error → direct
        `RESERVED` insert with the original wire (no queue fields).
  - [x] **Drain fallback**: `identity-chain-writer` promotes stranded
        `QUEUED` rows itself once the lease has been expired/absent longer
        than `QUEUE_FALLBACK_AFTER_SECS` (default 60) — claims queued before
        the service died still register.
  - [x] Boundary script covers the new service (no signing/JWT secrets);
        docs (`architecture.md`, `AGENTS.md`, `deploy.md`) updated.
- **✅ Demo**: `queue_live` pins both probes (lease fresh → claims stay
  queued; lease gone → `fallback_drain` promotes); the bin smoke-boots,
  acquires its lease, and the row reads alive=t. Stopping the compose
  `registration-queue` service leaves claims registering via the direct path.
- **Risks / failure modes**: clock-skew-free by construction (all liveness
  math is Postgres `now()`); a paused-but-alive advancer (holding the lease
  while wedged) would keep the queue lane open — mitigated by the lease being
  renewed in the same loop iteration that advances the queue.

## Review hardening (independent review, applied 2026-07-27) ✅
- [x] **Long iterations can't lapse the lease**: the advancer renews
      concurrently with the iteration work (`lease_ttl / 3` ticker, the
      writer-submit pattern), and group refresh reads balances 16-wide
      instead of serially — a large queue no longer stretches an iteration
      past the TTL and triggers the very fallback it guards against.
- [x] **`lease::renew` refuses an expired lease** (`expires_at > now()`
      guard): a stalled advancer can no longer silently resurrect a lease
      the fallback paths may already have acted on — it re-acquires a fresh
      epoch (also applies to the chain-writer's heartbeat).
- [x] **Group refresh preserves groups on balance-read failure** (fail-open
      to 1 is intake-only): a chain blip no longer demotes every queued
      claim to lowest priority.
- [x] **Absent-lease drain semantics made explicit**: grace applies to an
      expired-but-present lease (normal restart); an absent row (never
      deployed / state wiped) drains immediately — docs, code comments, and
      the live test now say the same thing.
- [x] **Unique default lease holder-ids** (`<bin>-<pid>-<random>`): container
      PID 1 no longer collapses overlapping instances onto one holder-id
      that could steal a live lease via the "already ours" arm.
- [x] **Fail-fast config** for the new `QUEUE_*` vars (`env_u64_strict`):
      garbage values abort startup instead of silently running on defaults.
- [x] **Intake balance read timeboxed** (5s): a hung RPC degrades the claim
      to group 1 instead of holding the request open.
- [x] **Simulation⇄SQL agreement pinned**: `queue_live` now asserts every
      promoted id's `drain_estimate` (position, iteration) equals the actual
      `advance_iteration` order on the same data; grace-window and
      renew-refuses-expired behavior also live-tested.

## Review hardening, round 2 (third independent review, applied 2026-07-27) ✅
- [x] **"Lease fresh ⇒ queue drains" actually enforced**: the group refresh
      (the only chain-dependent step) gets a hard deadline (`lease_ttl`);
      on timeout the iteration advances with current groups — a degraded
      RPC now stalls priorities for one interval, never draining, so the
      concurrent renewal can no longer mask a stuck advancer.
- [x] **Cadence validation matches the renewal scheme** (`validate_cadence`,
      unit-tested): the longest un-renewed gap is `interval + ttl/3`, so
      configs like interval 25/ttl 30 (which would flap a healthy lease
      every iteration and invite the fallback drain) are rejected at
      startup, as is a zero interval (hot loop).
- [x] **Queue-status endpoint rate-limited** per subject like the rest of
      the authenticated usernames surface, in its own bucket so polling
      never eats the claim quota (429 + `Retry-After`; O(queue) work per
      hit was otherwise unmetered).
- [x] **`queue_group` range enforced at the schema** (migration `0005`,
      `CHECK (queue_group BETWEEN 1 AND 4)`): the promotion SQL compares
      the raw column while the API snapshot clamps, so an out-of-range
      value could strand a row that the API still reports — now impossible
      to store.
- [x] **Boundary script pins the fallback wires**: `QUEUE_ENABLED` on
      identity-api and `QUEUE_FALLBACK_AFTER_SECS` on the writer are now
      required compose keys, so a compose refactor can't silently drop the
      resilience knobs.

## Review hardening, round 3 (fourth independent review, applied 2026-07-27) ✅
Verdict was "sound — no correctness/resilience/wire defects"; the residual
findings were visibility and hygiene:
- [x] **Flag-flip semantics documented end-to-end**: the queue-status route
      exists only when `QUEUE_ENABLED` (a deliberate, replay-pinned choice);
      the OpenAPI path description now says so and tells clients to treat
      ANY 404 as "no longer queued, registration proceeding" (queued rows
      keep draining after a flag flip — promotion never consults the flag).
      Operational sequence for retiring the queue recorded in
      `architecture.md` (flag off → drain → stop advancer).
- [x] **OpenAPI drift fixed**: the path item no longer implies the endpoint
      exists on queue-disabled deployments (the shipped default); artifacts
      regenerated.
- [x] **`queue_live` self-cleans at start** (leftover `QUEUED` rows +
      advancer lease from an aborted run no longer poison the slot-order
      assertions).
- [x] **Known limitation recorded** (architecture.md + endpoint doc): an
      account with several queued claims sees only the earliest-enqueued
      claim's standing — inherent to the spec's single-object response.

# Contracts (guardrails)

- `QUEUE_ENABLED=false` (default) ⇒ the entire HTTP surface and outbox flow
  are byte-identical to today (the `legacy_wire` replay suite must stay
  green with no fixture changes).
- All-statuses discriminator uniqueness holds: `QUEUED` rows keep their
  `full_username` allocated for availability and conflict checks
  (`allocated_discriminators` is status-blind).
- The chain-writer's contract is untouched: it only ever sees
  `RESERVED`/`RETRY_AFTER`/`SUBMITTING` rows; promotion is the only
  `QUEUED`-exit.
- Spec slot algebra: per iteration at most 4 promotions; slot k eligible
  groups `≥ 5−k`; earliest `(created_at, id)` wins; picked rows leave the
  pool before the next slot; empty eligible set skips the slot.
- **Queue-service unavailability degrades, never blocks**: with the advancer
  lease stale, new claims insert `RESERVED` directly (original wire) and
  already-queued claims are drained by the writer after the grace window —
  no claim is ever stranded behind a dead queue service.
- Group boundaries in planck (10-decimal DOT): `<10` → 1, `≥10` → 2,
  `≥100` → 3, `≥1000` → 4.

# Key decisions (decide early)

- **Queue advancer topology (was architecture.md open decision 1) — decided
  by the user 2026-07-27:** the queue is its own runtime service
  (`registration-queue` bin, still inside the identity-service crate/secret
  boundary) and an **isolated failure domain**: its lease doubles as the
  liveness signal, and both intake and the writer fall back to the original
  direct registration path when it is not alive. A queue outage degrades
  priority fairness, never availability of registration.
- **Estimates are a deterministic drain simulation** of the current snapshot
  (not a formula): exact w.r.t. the spec's slot rules, cheap (O(n) integer
  work), and directly unit-testable. `queuePosition` = pick order,
  `estimatedIterationsRemaining` = iteration in which the caller is picked.
- **Balance monitoring is per-iteration polling**, not event subscription:
  "immediately on confirmed deposit" is approximated to within one advance
  interval (≤ N seconds), which the spec's own cadence already tolerates.
- **The queue keys on the JWT subject** (`account_id`, the stored abuse-control
  key): the spec's "Backend monitors accountId balance" and the queue-status
  auth ("backend resolves accountId from JWT") both name the subject account,
  not the beneficiary.

# Testing

- Unit (in `just check`): group thresholds; drain-simulation slot algebra
  (FIFO within group, slot skipping, G4 monopoly, doc example); wire shapes
  of the new response fields; disabled-flag default.
- Live Postgres (`just test-live`): `queue_live` — enqueue across groups,
  `advance_iteration` promotes exactly the spec's picks to `RESERVED`,
  repeat to drain; group refresh update.
- Replay: existing `legacy_wire` suite unchanged (proves the contract above).
- Manual: compose demo per phase (claims queued → advancer drains → status
  endpoint counts down).

# Polish rounds (after MVP)

## Polish round 1: cutover envelope
- Adopt the spec's `200 {registrationOutcome}` response shape together with
  the attestation slice's INSTANT/PAYMENT lanes and client coordination.
- ✅ Check-in demo: updated apps drive claim → queue → registration against
  the new envelope.

# Later / Deferred

- INSTANT (voucher) and PAYMENT_REQUIRED lanes + `payment-status` endpoint —
  attestation/PoUD slice. Note for that slice: `GET
  /api/v1/usernames/payment-status` currently matches the gateway's
  `@username_reads` matcher (GET on `/api/v1/usernames/*` → indexer) and
  will need its own gateway route to identity-api.
- Queue metrics/alerts (depth, wait time per group) — production-hardening
  slice.
- Event-driven balance updates (storage subscription) — only if per-iteration
  polling proves too coarse in practice.
- Queue-status snapshot caching / SQL-side position query — the endpoint
  fetches all `QUEUED` rows per poll (O(queue × pollers)); optimize when
  measurements at real queue depth demand it (review finding, deferred per
  the "measured optimization waits" principle).
