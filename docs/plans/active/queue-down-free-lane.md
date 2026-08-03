# Queue-down policy — stop the free lane from silently unthrottling

> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

Follow-up to the registration-queue slice (#16) and the issue raised against
its fallback design. Today, with `QUEUE_ENABLED=true` and the advancer lease
stale, intake falls back to direct `RESERVED`
(`crates/identity-service/src/usernames/register.rs:355-366` — including on a
mere liveness-probe *error*) and the chain-writer drains stranded `QUEUED`
rows after `QUEUE_FALLBACK_AFTER_SECS`
(`crates/identity-service/src/chain/writer.rs:235-245`,
`queue::fallback_drain`). An **absent** lease row (advancer never deployed /
state wiped) drains with no grace at all. The queue is the throughput control
for free registrations; the current fallback turns "queue service down" into
"free registration as fast as you can call the endpoint", silently.

This plan **amends the 2026-07-27 key decision** in
`docs/plans/active/registration-queue.md` ("a queue outage degrades priority
fairness, never availability of registration"): while the queue is enabled,
throttling wins over registration *progress*. Intake availability is kept —
claims still land durably — but they wait.

# Goals

- With `QUEUE_ENABLED=true`, a free-lane claim is **always** inserted as
  `QUEUED` — advancer alive, dead, or never deployed. No code path silently
  reverts to unthrottled direct `RESERVED` intake while the flag is on.
- With `QUEUE_ENABLED=true` and the advancer down, already-queued rows **stay
  queued** (durable, resumed when the advancer returns) instead of being
  auto-drained by the writer.
- A stalled queue is **observable**: the writer logs stranded-row warnings and
  the runbook carries the alert condition, so "queue down" is an operator page,
  not a silent behavior change.
- The queue-disabled contract is preserved: `QUEUE_ENABLED=false` still never
  strands a row (the writer's drain becomes the flag-off janitor instead of a
  liveness fallback).

Success is binary: the Phase 1/2 demos below — claim queues and *stays*
queued past the grace window with the advancer stopped and the flag on;
flag off drains. Proven by `queue_live` assertions and a compose demo.

# Non-goals

- No wire changes. The claim response (`202`, `registrationOutcome: "QUEUED"`,
  `queue` standing) and `GET /api/v1/registration/queue` keep their exact
  shapes; the `QUEUE_ENABLED=false` surface stays byte-identical
  (`legacy_wire` replay suite untouched).
- No client coordination — parking is invisible to clients beyond a longer
  wait (they already poll the status endpoint).
- No new throttling mechanism (rate limits on the direct path, etc.) — the
  queue *is* the throttle; this plan just stops bypassing it.
- Queue metrics/alerting infrastructure — stays with the production-hardening
  slice; this plan only adds the log lines and runbook entry.
- Rejecting claims (503) while the queue is down — see Key decisions.

# Design principles

- User journey drives order.
- **The flag is the policy**: `QUEUE_ENABLED` alone decides whether free
  claims are throttled. Service liveness affects *progress*, never *policy*.
- Durable over dropped: a claim accepted while the queue is stalled must
  survive and register later, in fair order.
- Reuse before rebuild: `fallback_drain`, the lease machinery, and the
  outbox states are all kept — only the *conditions* under which they act
  change.

# User journey

1. A free-lane user posts a claim while the queue service happens to be down.
2. They get the same `202 QUEUED` response as always, with their queue
   standing.
3. They poll `GET /api/v1/registration/queue` and see their position hold
   steady (nothing drains) instead of silently registering unthrottled.
4. Ops gets paged on the stranded-queue signal, restarts the advancer, the
   queue drains in fair slot order, and the user's registration proceeds.
5. If the operator instead retires the queue (`QUEUE_ENABLED=false`), the
   writer's janitor drains the remainder and nothing strands.

# Foundations / Already shipped (✅)

## Queue intake, advancer, and status (registration-queue slice, 2026-07-27)
- What exists: `QUEUED` outbox intake with balance-priority groups
  (`register.rs`, `queue::intake_group`), the single-instance advancer bin
  with lease-as-liveness (`queue::run_advancer`,
  `src/bin/registration_queue.rs`), the drain-simulation status endpoint,
  and `queue_live` Postgres tests including the current fallback assertions.
- ✅ Demo: `just test-live` (`queue_live`), compose demo in the
  registration-queue plan.
- Gaps: the two fallbacks this plan re-scopes.

## The two fallbacks (the code being changed)
- What exists: intake's `queue::advancer_alive` probe gating the queue lane
  (`register.rs:359-366`); the writer's per-tick
  `queue::fallback_drain(pool, queue_fallback_after)` with grace-window and
  absent-lease-drains-immediately semantics (`queue.rs:371-393`,
  `writer.rs:61-63,107,235-245`); `scripts/verify_compose_boundaries.sh`
  pinning the fallback env wires; the behavior codified in
  `docs/architecture.md` ("The queue service can be down without bricking
  registration") and the registration-queue plan's contracts.
- ✅ Demo: `queue_live` "lease gone → fallback_drain promotes" today.
- Gaps: both act while `QUEUE_ENABLED=true`, defeating the throttle.

# MVP phases (ship-shaped, demoable)

## Phase 1: intake always queues while the flag is on — code-complete 2026-08-03
- **Goal**: no claim enters the direct lane while `QUEUE_ENABLED=true`.
- **Scope checklist**:
  - [x] (2026-08-03) `register.rs`: `queue_lane = state.config.queue_enabled`
        — dropped the `advancer_alive` probe (and with it the probe-error →
        direct path). `intake_group`'s fail-open-to-group-1 behavior
        unchanged.
  - [x] (2026-08-03) `queue::advancer_alive` kept but demoted to
        observational (stranded-queue signal + tests); doc comment rewritten.
  - [x] (2026-08-03) Module docs rewritten: `queue.rs` header, `register.rs`
        header + lane comment, `registration_queue.rs` bin header.
  - [x] (2026-08-03) Live coverage: `queue_live` asserts `stranded_queued`
        counts rows behind an absent AND an expired lease, and is 0 with a
        live lease. (The handler-level "flag on + no lease → 202 QUEUED"
        path is the compose demo below — the offline test app has no
        DB/chain for the standard lane.)
  - [x] (2026-08-03) Same-change docs: `docs/architecture.md` queue
        invariant blocks rewritten, registration-queue plan contract +
        key-decision amendment recorded, `.env.example` wording updated.
- **✅ Demo**: compose stack with `registration-queue` **stopped** and
  `QUEUE_ENABLED=true`: `POST /api/v1/usernames` → `202` with
  `registrationOutcome: "QUEUED"`; the row is `QUEUED` in Postgres; the
  status endpoint reports a position; the row is **still** `QUEUED` minutes
  later. Start the advancer → it drains in slot order.
- **Risks / failure modes**:
  - Misconfig (flag on, advancer never deployed) now stalls free
    registration instead of silently unthrottling — intended, but it must be
    loud: Phase 2's stranded-row warning + runbook entry are the
    compensation. Ship Phase 2 in the same release.
  - `estimatedIterationsRemaining` is meaningless while nothing drains;
    accepted (deterministic simulation, documented), revisit in polish.

## Phase 2: the writer drain becomes the flag-off janitor — code-complete 2026-08-03
- **Goal**: stranded `QUEUED` rows are drained only when the queue is
  *disabled*, never as a liveness fallback while it is enabled.
- **Scope checklist**:
  - [x] (2026-08-03) Writer `Config` gains `queue_enabled` (same
        `QUEUE_ENABLED` env via the shared strict `env_bool`, default off,
        parsed alongside `queue_fallback_after`).
  - [x] (2026-08-03) Writer loop: `fallback_drain` only when
        `queue_enabled=false`; the lease-stale + grace condition inside
        `fallback_drain` is unchanged (a live advancer still finishes a fair
        drain first; the concurrent promote/drain race stays benign).
  - [x] (2026-08-03) Flag-on stranded warning: new `queue::stranded_queued`
        (QUEUED rows behind an absent/expired lease), checked by the writer
        at the `queue_fallback_after` cadence and logged as a `warn!` with
        the count and the operator actions.
  - [x] (2026-08-03) `docker-compose.yml` passes `QUEUE_ENABLED` to
        `identity-chain-writer`; `scripts/verify_compose_boundaries.sh`
        requires it (script green).
  - [x] (2026-08-03) `queue_live` reframed: the drain assertions are
        explicitly the flag-off janitor semantics; `stranded_queued`
        asserted ≥1 behind absent and expired leases, 0 with a live lease.
        (The writer-loop gating itself is a trivial `if` — exercised by the
        compose demo, not a live test.)
  - [x] (2026-08-03) `docs/runbook.md`: stranded-queue warning failure mode
        (restart vs retire) + the split-`QUEUE_ENABLED` misconfig row.
- **✅ Demo**: compose stack, `QUEUE_ENABLED=true`, advancer stopped, rows
  queued: past `QUEUE_FALLBACK_AFTER_SECS` the rows are still `QUEUED` and
  the writer logs the stranded warning. Set `QUEUE_ENABLED=false` on the
  writer and restart it → rows drain to `RESERVED` and register (the
  original janitor behavior, now correctly scoped).
- **Risks / failure modes**:
  - Rows queued forever if ops ignores the warning with the advancer
    unrecoverable — the escape hatch is deliberate and manual: flag off
    (janitor drains) — no automatic path back to unthrottled.
  - Mixed `QUEUE_ENABLED` across identity-api / writer during a rolling
    flip: intake and janitor disagree for one deploy window. Documented in
    the runbook (flip writer last when disabling); self-heals once
    consistent.

# Contracts (guardrails)

- **New**: `QUEUE_ENABLED=true` ⇒ every free-lane claim inserts `QUEUED`;
  nothing but advancer promotion moves it out while the flag stays on.
  Replaces "Queue-service unavailability degrades, never blocks" in the
  registration-queue plan and the architecture invariant "the queue service
  can be down without bricking registration" — rewrite both in Phase 1, plus
  the absent-lease paragraph (absence no longer drains while the flag is on).
- Preserved: `QUEUE_ENABLED=false` (default) ⇒ byte-identical wire
  (`legacy_wire` replay suites green, no fixture changes) and no stranded
  rows (janitor drain).
- Preserved: the chain-writer's promotion contract — it only ever *writes*
  `QUEUED→RESERVED` via `fallback_drain` (now flag-off only); it never
  submits `QUEUED` rows to chain.
- Preserved: voucher `INSTANT` and payment lanes bypass the queue by design
  (spec precedence, `eligibility.rs`); this plan touches only the free lane.
- Preserved: slot algebra, group thresholds, status-endpoint semantics
  ("any 404 = not queued, proceeding") — all unchanged.

# Key decisions (decide early)

- **Park, don't reject (recommended, needs sign-off — this is the issue's
  open question).** Three candidate policies for "flag on, advancer down":
  1. **Park as `QUEUED`** (this plan): intake stays available and
     wire-identical, claims are durable and drain fairly on recovery, no
     client work. Cost: registration *progress* stalls until ops acts.
  2. Reject free claims (503/`Retry-After`): truer "skip free registration",
     but a new wire behavior on a frozen surface, drops durable intake, and
     unknown client handling. Rejected for now.
  3. Status quo (silent direct): rejected — it is the issue.
  Parking matches the issue's intent ("skip free registration instead of
  silently going direct") without a wire change; if product later wants
  hard rejection, it layers on top of Phase 1 trivially.
- **The break-glass for a dead-forever advancer is the existing flag**, not
  a new env: `QUEUE_ENABLED=false` → janitor drains. No separate
  `QUEUE_FORCE_DRAIN` knob (one less wire to pin; the flag flip is already
  the documented retire path).
- **`QUEUE_FALLBACK_AFTER_SECS` survives** as the janitor's
  defer-to-live-advancer grace during the retire sequence; its env contract
  and default (60) are unchanged.
- **Amends the 2026-07-27 topology decision's fallback clause** — record the
  amendment in `docs/plans/active/registration-queue.md` when this ships, so
  the two plans don't state contradictory contracts.

# Testing

- Unit: lane selection is flag-only (no liveness input left to test);
  writer janitor gating by `queue_enabled`.
- Live Postgres (`just test-live`, `queue_live`): flag-on + expired lease +
  past grace ⇒ rows stay `QUEUED`; flag-on + absent lease ⇒ intake queues,
  no drain; flag-off ⇒ existing drain assertions; advancer restart drains in
  slot order (existing).
- Replay: both `legacy_wire` suites untouched and green (queue-disabled
  wire frozen).
- Boundary: `scripts/verify_compose_boundaries.sh` green with the writer's
  `QUEUE_ENABLED` wire added.
- Manual: the two compose demos above.

# Polish rounds (after MVP)

## Polish round 1: stalled-queue visibility for clients and ops
- Consider surfacing "queue stalled" (advancer lease stale) on the status
  endpoint or `/readyz` so clients/ops see it without log access — additive
  field, needs a wire ruling, deliberately not in the MVP.
- ✅ Check-in demo: with the advancer stopped, the chosen surface reports the
  stall; with it running, reports healthy.

# Later / Deferred

- Queue depth / wait-time metrics and real alerting — production-hardening
  slice (unchanged from the registration-queue plan's deferral).
- Hard-reject policy (503 on free claims while stalled) — only if product
  decides parked-but-stalled is unacceptable UX; layers on Phase 1.
- Auto-recovery beyond restart (e.g. supervised advancer respawn) — ops
  concern, out of scope here.
