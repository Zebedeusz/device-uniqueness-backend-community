> Stage: **active**. Keep this plan current while working: when a scope item is finished, check its
> box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is
> the source of truth, not memory.

> **Superseded in part (2026-08-18).** The V1 wire dialects this plan captured and froze were
> removed: every service now renders one JSON error envelope (`{error}`, plus `fields` on per-field
> validation), and the `legacy_wire.rs` replay suites plus their `legacy-wire.json` fixtures are
> deleted. Phases below that describe capturing, freezing, or replaying those bodies are historical
> record, not current behavior — see `CHANGELOG.md` (Unreleased → Removed).

# Eligibility, payment & registration vouchers — INSTANT / PAYMENT_REQUIRED lanes

Implements remaining-scope item 2 of `docs/architecture.md`: the INSTANT (voucher) and
PAYMENT_REQUIRED (paid-fallback) registration lanes from Mike's "New JWT / Integrity / PoUD /
Username Claim Logic" spec, the `payment-status` polling endpoint, and the groundwork for the
spec's `200 {registrationOutcome}` claim envelope. Everything lives **inside `device-attestation`**
(architecture guardrail: `eligibility`, `payment`, `queue`, `writer` are modules, never services).

**Checked against Mike's spec doc + overview diagram 2026-07-30** (the full 27-page "New JWT /
Integrity / PoUD/Username Claim Logic" PDF): endpoint shapes below are spec-exact where noted;
the two recorded deviations are the envelope timing (additive-202 now, universal-200 later — see
Key decisions) and cumulative-balance detection (Phase 3). The spec's queue tables match the
shipped queue slice; the diagram's queued-user "speed up by deposit" is the top-up/priority-group
mechanism the queue slice already implements. Per the user's direction (2026-07-30), the doc's
**WEB3Summit chapter** (two-step `/auth/android/attestation`, its failure table, the
`verifiedBootKey` POUD note) **and the Play Integrity self-managed-keys chapter are ignored** —
the alignment check covers the main Username Registration spec only (legend, overview diagram,
TEE attestation validation, endpoints 1–3, queue tables); the TEE attestation section itself is
the attestation slice's territory. The doc has **no** voucher record/mint-endpoint section —
those details exist only in the threat model's digest of the proposal's other tab, which this
plan follows (with the M1 hash-at-rest fix).

**Ground truth established 2026-07-30** (legacy survey): legacy shipped the full
INSTANT/QUEUED/PAYMENT_REQUIRED decision as real, property-tested code
(`decide-claim.workflow.ts`, `lifetime_poud_vouchers` table, atomic single-use redeem) — but the
HTTP route was **never mounted**; no client has ever seen the rich envelope. Everything
payment-side is spec-only: no `payment-status` route, no payment table, no deposit watcher; the
"payment address" is a hardcoded Alice address with amount 0, and legacy INSTANT is a no-op
(`InstantClaim.Default = Effect.void` — burns the voucher, registers nothing). The shipping iOS
app decodes only `{base_username, digits, username}` from the claim response
(`ClaimUsernameModels.swift`) and has **no** `registrationOutcome` handling. So: the decision
*logic* and voucher *redeem semantics* are ports; every wire surface beyond today's frozen one is
greenfield we are free to shape — feature-gated, spec-shaped, coordinated with clients later.

# Goals

- A username claim resolves through **one typed eligibility decision** —
  `INSTANT | QUEUED | PAYMENT_REQUIRED` — replacing the two hardcoded
  `{"registrationOutcome":"PAYMENT_REQUIRED"}` literals in `usernames/register.rs`.
- **Voucher lane (INSTANT)**: a `lifetimePoUDVoucher` body field on `POST /api/v1/usernames`
  redeems an admin-minted, single-use, expiring voucher and registers immediately (skips PoUD
  *and* the queue), actually reaching the chain via the existing outbox → writer path.
- **Payment lane (PAYMENT_REQUIRED)**: a blocked claim returns a unique payment address + required
  amount; a chain-confirmed deposit flips the stored claim into the outbox and the username
  registers with no further client action.
- **`GET /api/v1/usernames/payment-status`** (JWT): pending/confirmed/expired polling for the
  caller's active payment request.
- All of it **feature-gated off by default** — with the new flags off, the wire stays
  byte-identical to today (frozen 202 body, bare DeviceCheck `PAYMENT_REQUIRED`, 55 fixtures
  green).

# Non-goals

- **Android PoUD identifiers.** Deferred with cause: no proposed identifier satisfies all three
  required properties (TEE-bound, per-device stable, per-device unique) — the findings report
  documents the three attempts: `androidId`+`widevineId` (legacy's `android_device_identifiers`
  OR-match; client-supplied, so a modded client forges fresh ones — fails binding),
  `verifiedBootKey` (it is the OEM boot-signing key, identical
  across a whole device line — first registrant locks out the OEM's every other user), and
  `cert[1].publicKey` (RKP certs rotate ~biweekly — fails stability). Until a workable
  identifier exists this is a product decision, not a port. The eligibility decision's `device`
  input is the seam it would plug into (see Later).
- **The auth voucher** (`Auth-Attestation-Type: voucher` on `/auth/token`) — a different feature
  with a different table (`voucher_secrets`), owned by its own follow-up slice per the attestation
  plan. Do not conflate: this slice's voucher is the *registration* voucher
  (`lifetime_poud_vouchers` lineage, `lifetimePoUDVoucher` field).
- **Flipping the claim success to the spec's universal `200 {registrationOutcome}` envelope.**
  Shipping clients hard-decode the 202 three-key body; the flip needs client releases and ships as
  a coordinated polish round together with attestation Phase 5 (attestation plan deviation #7).
- Refunds, double-payment recovery, automated fund sweeping — ops/runbook territory (see Later).
- Queue mechanics changes. The queue slice is done; this slice only *routes into* it.

# Design principles

- User journey drives order.
- **One decision, three lanes, one outbox.** The eligibility decision is a pure function; every
  lane exits through the existing `username_reservations` outbox (`RESERVED` insert) — no second
  registration pipeline, no second chain writer (payment detection is read-only).
- **Additive wire first, envelope flip later.** The 202 body already carries additive
  `registrationOutcome: "QUEUED"` + `queue` fields behind `QUEUE_ENABLED`; INSTANT rides the same
  pattern. The rich `PAYMENT_REQUIRED` body extends the existing 200. Nothing existing changes
  shape until the coordinated flip.
- **Voucher precedence** (spec FR-009, legacy `decideClaim` order): the voucher is checked before
  DeviceCheck/PoUD — a valid voucher on a "seen" device still registers, and no DeviceCheck bit is
  spent; a spent/unknown/expired voucher **rejects the claim** (400), it never falls through to
  another lane.
- **Secrets discipline** (threat model M1): voucher keys are stored **only as sha256 hashes**;
  plaintext exists once, at mint output. The payment scheme keeps zero private keys in the
  service.
- Fail closed on money, fail open on nothing: a payment is confirmed only by observed chain state;
  an unreadable balance never confirms and never errors the poll — it stays `pending`.

# User journey

1. User claims a username (`POST /api/v1/usernames`, JWT + candidateSignature as today).
2. The eligibility decision resolves the lane:
   - valid `lifetimePoUDVoucher` → **INSTANT**: reservation inserted directly (`RESERVED`),
     202 + `registrationOutcome: "INSTANT"`; voucher burned atomically.
   - PoUD pass (fresh device / soft posture) → **QUEUED** or direct reservation, exactly as
     today.
   - PoUD fail (seen device; later: `appFromOfficialStore=false`) → **PAYMENT_REQUIRED**:
     `200 {registrationOutcome, paymentAddress, amountRequired}`; a pending payment request row
     stores the fully-validated claim.
3. Paying user sends the amount to the address; the watcher observes the deposit, flips the
   request `CONFIRMED`, and inserts the stored claim as `RESERVED` — the writer registers it
   on-chain like any other reservation.
4. The app polls `GET /api/v1/usernames/payment-status` and sees `pending` → `confirmed`;
   the username appears in search like any registration.

# Foundations / Already shipped (✅)

## Claim path with an eligibility seam
- What exists: `usernames/register.rs` — validation → candidateSignature → digit selection →
  **DeviceCheck gate** (`register.rs:234-261`, iOS hard mode → bare
  `200 {"registrationOutcome":"PAYMENT_REQUIRED"}` at `:244` and `:339`) → queue-lane decision
  (`:283-295`, `QUEUE_ENABLED` + advancer lease) → `reserve()` → outbox insert. The 202 body
  (`RegisterResponse`, `:113-147`) already has additive `registration_outcome` /
  `queue` / `device_check_available` fields, all `skip_serializing_if`.
- ✅ Demo: 55-fixture replay green (`tests/legacy_wire.rs`); the parked
  `register-200-devicecheck-payment-required` fixture pins the bare 200.
- Gaps: the two hardcoded JSON literals; no INSTANT; PAYMENT_REQUIRED is a dead end (nothing
  stored, no way to pay).

## Outbox + writer + queue
- What exists: `chain/outbox.rs` state machine (`QUEUED → RESERVED → SUBMITTING → ASSIGNED |
  RETRY_AFTER | FAILED_TERMINAL`), single-instance `device-attestation-chain-writer`
  (`chain/writer.rs`, lease + `fallback_drain`), queue advancer (`queue.rs`), migrations
  `0001`–`0005` (next: `0006`).
- ✅ Demo: registration-queue plan phases 1–4 done 2026-07-27; `just test-live` queue suite.

## Chain reads
- What exists: `ChainClient::free_balance(account: [u8;32]) -> u128` (`chain/client.rs:337-348`),
  used by queue grouping; takes any account — directly reusable for payment detection. No
  event/transfer watching exists (and none is needed under the balance-polling decision below).

## DeviceCheck gate (attestation Phase 4)
- What exists: `device_check::decide_gate` pure decision table + under-lock re-query in the claim
  path; `Decision::Blocked` is the current PAYMENT_REQUIRED trigger.

## Legacy reference (semantics to port, not wire to freeze)
- What exists: legacy `decide-claim.workflow.ts` (+ property tests) — voucher precedence and
  reject-on-spent semantics; `voucher.store.ts` — atomic single-use
  `UPDATE … WHERE key=? AND used=false RETURNING`; the unmounted route's rich envelope
  `{registrationOutcome, queuePosition, paymentAddress?, amountRequired?}` (amount as planck
  string) as a shape reference. None of it is client-observed; V2 follows the spec + its own
  dialect where they differ.

# MVP phases (ship-shaped, demoable)

## Phase 1: Eligibility module + voucher lane (INSTANT) — code-complete 2026-07-30
- **Goal**: one typed decision replaces the scattered gate logic; a voucher claims instantly.
- **Scope checklist**:
  - [x] (2026-07-30) `eligibility` module in `device-attestation`
        (`src/eligibility.rs`): pure `decide(Option<VoucherState>) -> Lane | VoucherError`
        (voucher precedence; bad voucher rejects, never falls through), `voucher_state`,
        atomic `redeem_and_reserve`; unit tests over the full decision table + pinned sha256
        vector. (Phase 2 grows `decide` with the device-gate/payment inputs.)
  - [x] (2026-07-30) Migration `0006_registration_vouchers.sql`: `registration_vouchers`
        (`key_hash` BYTEA PK, `minted_batch`, `created_at`, `expires_at`, `used_at`).
        Hash-only at rest (threat model M1).
  - [x] (2026-07-30) `lifetimePoUDVoucher` on `POST /api/v1/usernames`: flag off → ignored
        (pinned by unit tests + the 55-fixture replay staying green untouched); flag on →
        recognised (empty string = absent, wrong type = zod field error), state lookup before
        the DeviceCheck gate, conditional-UPDATE burn in the same transaction as the
        reservation insert (username conflict rolls the burn back).
  - [x] (2026-07-30) INSTANT exit: direct `RESERVED` insert (skips the queue even with
        `QUEUE_ENABLED`), 202 + additive `registrationOutcome: "INSTANT"`, no queue fields, no
        DeviceCheck bit spent. Errors: 400 `{"error": "Voucher not found" | "Voucher already
        used" | "Voucher expired"}` (new surface, no frozen dialect).
  - [x] (2026-07-30) `voucher-mint` operator CLI (`src/bin/voucher_mint.rs`):
        `--count N --ttl-days D [--batch LABEL]`, all-or-nothing insert, plaintext keys printed
        once to stdout, hashes + batch label persisted. No HTTP mint surface.
  - [x] (2026-07-30) Config `REGISTRATION_VOUCHERS_ENABLED` (strict `env_bool`, default false)
        in `Config` + `test_default()` + `.env.example`; OpenAPI regenerated (`just openapi`);
        no new fixtures needed — the frozen 55 replay unchanged, which is itself the flags-off
        pin.
- **✅ Demo (passed 2026-07-30, scratch Postgres in Docker + live Paseo Next v2 RPC)**:
  `voucher-mint --count 2` printed two keys (hashes only in the DB); the live HTTP test
  (`tests/voucher_http_live.rs`, production router) claimed with a voucher while
  `QUEUE_ENABLED=true` → `202 {registrationOutcome: "INSTANT"}`, row born `RESERVED`, no queue
  fields; voucher reuse → `400 Voucher already used`; unknown voucher → `400 Voucher not
  found`. `tests/voucher_live.rs` proves redeem atomicity: concurrent redeems of one voucher
  admit exactly one; a username conflict preserves the voucher; expired vouchers refuse.
  `just check` green (fmt + clippy `-D warnings` + workspace tests incl. the frozen replay).
  *Remaining demo gap: watching the writer take the INSTANT row to `ASSIGNED` on chain — the
  writer path is unchanged from `RESERVED` on and covered by the earlier slices' demos; re-run
  as part of the migration-wide parity gate.*
- **Risks / failure modes**: redeem/reserve atomicity (burn must not survive a failed insert —
  same transaction; live-tested); voucher errors must not leak whether a key exists vs is spent
  beyond what the two error bodies already say.

## Phase 2: Payment quote lane (PAYMENT_REQUIRED becomes actionable) — code-complete 2026-07-30
- **Goal**: a blocked claim yields an address + amount and durably stores the claim.
- **Scope checklist**:
  - [x] (2026-07-30) Migration `0007_payment_requests.sql`: `payment_requests` — `account_id`
        (one **PENDING** per subject via partial unique index), `derivation_index`
        (u16-checked, allocated from a `MAXVALUE 65535 NO CYCLE` sequence — exhaustion is a
        loud failure), `payment_address`, `amount_planck` (frozen at quote time),
        `PENDING | CONFIRMED | EXPIRED | FAILED_CONFLICT`, `expires_at`, `confirmed_at`, and
        the validated claim payload (**minus the digit selection** — deliberate improvement on
        the drafted shape: digits re-select at confirmation so a quote held for days rarely
        conflicts; `preferred_digits` stored and honored if still free).
  - [x] (2026-07-30) `src/payment.rs`: `derivative_address` =
        `blake2_256(scale(("modlpy/utilisuba", master, index)))` — verified against the
        `pallet_utility::derivative_account_id` source and unit-pinned by SCALE-encoding the
        sdk's exact tuple via `codec::Encode`; SS58 render via `AccountId32` (substrate
        generic prefix, matching the API's dialect). Config block `PaymentConfig`
        (`PAYMENT_MASTER_ACCOUNT` SS58-validated, `PAYMENT_AMOUNT_PLANCK` > 0,
        `PAYMENT_REQUEST_TTL_SECS` default 86400) gated by `PAYMENT_LANE_ENABLED`
        (default false, fail-fast requirements like the DeviceCheck block).
  - [x] (2026-07-30) Claim path: `payment_required()` — lane off → the frozen bare 200 body
        (parked fixture untouched); lane on → `payment::quote` and
        `200 {registrationOutcome, paymentAddress, amountRequired}`. `amountRequired` ships as
        a **planck string** (the legacy unmounted shape) pending the client-team unit ruling —
        a one-line change at `register.rs::payment_required` if DOT wins.
  - [x] (2026-07-30) "No error codes" rule: lane on → hard-mode missing device token
        (`Decision::TokenRequired`) resolves to the payment quote; lane off → the
        legacy-parity 401. `Decision::Blocked` and the lost free-slot race
        (`DeviceAlreadyClaimed`) route through the same helper. DeviceCheck `Unavailable`
        stays 502 in every posture (infra failure, not eligibility — fail closed).
  - [x] (2026-07-30) Idempotent re-claim: the same address + frozen amount back, TTL
        refreshed, stored intent re-targeted (last intent wins); the sequence is consumed only
        on first mint, so polling re-claims never burn index space; concurrent first-claims
        resolved by the partial unique index + one retry.
- **✅ Demo (passed 2026-07-30, scratch Postgres in Docker + live Paseo Next v2 RPC)**: the
  live HTTP test (`tests/payment_http_live.rs`, production router, hard mode + DeviceCheck on
  + queue on) claimed with **no device token** → `200 {registrationOutcome:
  "PAYMENT_REQUIRED", paymentAddress, amountRequired}`, a durable `PENDING` row holding the
  claim intent; re-claim → the same address (no second quote). `tests/payment_live.rs` proves
  the quote semantics (frozen amount, TTL refresh, last-intent-wins re-target, one row per
  subject, address = derivative(master, stored index), distinct subjects → distinct
  addresses). Lane-off wire pinned by the untouched 55-fixture replay + parked
  `register-200-devicecheck-payment-required`. `just check` green; voucher suites re-run green
  after the handler restructure.
  Review follow-up (2026-08-03): the address scheme swapped from Utility derivative accounts to
  the keyless-dummy multisig, per-subject with no stored derivation input (see Key decisions,
  including the accepted stale-balance divergence) — migration `0007` rewritten in place (this
  slice is unreleased; scratch test databases need a reset), `derivation_index` + sequence
  dropped with nothing replacing them, the `payment::deposit_account` derivation unit-pinned
  against SCALE-encoding `pallet_multisig::multi_account_id`'s exact tuple (both sort orders),
  and `payment_live` re-derives the stored address from master + subject. Wire unchanged
  (`paymentAddress` stays an opaque SS58 string). The sweep runbook item (Polish round 2) is now
  `Multisig.as_multi_threshold_1` with the dummy re-derived per row — and sweep cadence is
  load-bearing for revenue, not just hygiene.
- **Risks / failure modes**: derivation-index collision under concurrent claims (serialize via
  the unique index + retry; sequence-allocated); storing the claim payload means the
  candidateSignature is validated **at claim time** and trusted at registration time —
  acceptable (same trust as the outbox itself, rows are server-owned).

## Phase 3: Payment detection + registration hand-off — code-complete 2026-07-30
- **Goal**: money on chain becomes a registered username with no client action.
- **Scope checklist**:
  - [x] (2026-07-30) Watcher in the **`device-attestation-chain-writer` active loop**
        (`payment::watch_pass`, called from `writer.rs::active_loop` every
        `PAYMENT_POLL_INTERVAL_SECS`, default 30; read-only on chain — nonce lane untouched;
        a no-op while `payment_requests` is empty, so the writer needs no payment gate):
        expire, then per PENDING row read `free_balance(payment_address)`;
        `balance >= amount_planck` → `confirm_request` — re-select the discriminator against
        fresh chain + outbox state (preferred honored if free, else random free; a paid
        registration never fails for digit contention short of full exhaustion), then
        `PENDING → CONFIRMED` + `RESERVED` outbox insert in **one transaction** (status-guarded
        flip = idempotent; insert conflict rolls the burn back and retries next pass). Base
        exhausted → `FAILED_CONFLICT`, the payment record kept for support.
        `payment::confirm_by_id` is the support-path entry for manually honoring a deposit the
        cumulative detection can't see.
  - [x] (2026-07-30) Expiry: `PENDING` past `expires_at` → `EXPIRED`
        (`payment::expire_pending`, same pass; spec FR-008/SC-006). A deposit observed on an
        `EXPIRED` request stays unconsumed (support/refund path — Later).
  - [x] (2026-07-30) Detection is **cumulative balance ≥ required** (the recorded divergence);
        an unreadable balance skips the row — never confirms, never errors the pass.
- **✅ Demo (passed 2026-07-30, scratch Postgres in Docker + live Paseo Next v2 RPC —
  `tests/payment_watch_live.rs`)**: confirmation hands off exactly once (request `CONFIRMED`
  with `confirmed_at`, reservation `RESERVED` as `base.07` — the preferred digits honored at
  confirmation; a second attempt finds nothing to do); an unfunded quote survives a full
  `watch_pass` untouched (`PENDING`, detection-negative against the live chain); a paid request
  on a fully-exhausted base goes `FAILED_CONFLICT` without losing the payment record; backdated
  quotes expire. `just check` green; all payment/voucher live suites re-run green.
  Review follow-up (2026-07-31): two more live tests — a taken preferred digit substitutes a
  random free one at confirmation (the divergence from the claim path's 409, previously only
  the preferred-honored case was covered), and a corrupted `payment_address` row is skipped
  instead of erroring the pass (rows are walked in `created_at` order, so an erroring row
  would have frozen detection for everything created after it — `watch_pass` now warns+skips,
  matching the balance-read-failure arm).
  Review follow-up (2026-08-03): the CONFIRMED/FAILED_CONFLICT flips now also guard on the
  snapshot's `updated_at` — a re-claim re-target between the watcher's `fetch_pending` and the
  flip previously registered (or exhausted) the **stale** intent; a mismatched flip now loses
  cleanly and retries next pass with fresh data. `PAYMENT_AMOUNT_PLANCK` is bounds-checked
  against BIGINT at config parse (the quote path's `i64` clamp is now unreachable). Coverage:
  the `FAILED_CONFLICT → CONFIRMED` status mapping is unit-pinned (`payment::wire_status`),
  and a new live test proves voucher precedence over the FR-005 non-store gate
  (`payment_http_live::a_valid_voucher_beats_the_non_store_payment_gate`).
  *Remaining demo gap (same as Phase 1's): a real transfer driving `balance ≥ amount` and the
  writer taking the row to `ASSIGNED` needs a funded signer on a dev chain — the detection
  comparison and the writer path from `RESERVED` on are each proven separately; run the full
  money-in → username-on-chain pass in the migration-wide parity gate. That pass MUST include
  a confirmation where the preferred digit was taken and a random one was substituted: the
  on-chain `attest` pairs `consumerRegistrationSignature` with the final `full_username`, and
  while the normal lane's server-side random digit selection implies the signature cannot
  cover the discriminator, only a substituted-digit registration reaching `ASSIGNED` proves
  it — a wrong assumption here surfaces as `FAILED_TERMINAL` after the user has paid.*
- **Risks / failure modes**: watcher crash between mark-CONFIRMED and reservation insert —
  prevented by single transaction (live-tested); chain RPC flap — poll skips, stays `PENDING`
  (fail closed).

## Phase 4: `payment-status` endpoint — code-complete 2026-07-30
- **Goal**: the wait is visible; the journey closes end-to-end from the app's point of view.
- **Scope checklist**:
  - [x] (2026-07-30) `GET /api/v1/usernames/payment-status` (`payment::status`; JWT via
        `AuthSubject`, own subject-keyed rate bucket like queue-status; mounted only when the
        payment lane is on — `usernames::router(payment_enabled)`, unmounted → the global Hono
        plain-text 404). Response **spec-exact**: `{"status": "PENDING" | "CONFIRMED"}`. No
        active request — including `EXPIRED` — → `404 {"error": "No active payment request"}`;
        the client re-claims for a fresh quote. **Decided mapping:** `FAILED_CONFLICT` reads as
        `CONFIRMED` — the deposit WAS observed; a 404 would push the client to re-claim and pay
        twice; the stuck registration is a support case.
  - [x] (2026-07-30) Gateway: `not path /api/v1/usernames/payment-status` inside
        `@username_reads` (`gateway/Caddyfile`) so the poll reaches device-attestation-api, not the
        indexer; `caddy validate` green (caddy:2-alpine); route table updated in
        `docs/plans/active/gateway.md`.
  - [x] (2026-07-30) Wire + OpenAPI: mounted-only-when-enabled pinned by
        `payment_status_route_is_mounted_only_when_payment_lane_enabled`
        (`tests/legacy_wire.rs`, the queue-mount test's pattern — no new captured fixtures: the
        surface has no legacy); `crate::payment::status` + `PaymentStatusResponse` registered
        in `openapi.rs`, artifacts regenerated (`just openapi`).
- **✅ Demo (passed 2026-07-30, scratch Postgres in Docker + live Paseo Next v2 RPC —
  `tests/payment_http_live.rs`)**: over the production router — quote issued → poll
  `{"status": "PENDING"}` (spec-exact byte shape asserted) → `confirm_by_id` →
  `{"status": "CONFIRMED"}` → rows cleaned → `404 {"error": "No active payment request"}`;
  lane-off → plain-text Hono 404 (wire test). All payment/voucher live suites re-run green;
  `just check` green. *Remaining demo leg: the same poll through the compose gateway container
  (proving the Caddyfile carve-out end-to-end) — `caddy validate` covers the syntax; run the
  routing smoke with the compose-stack pass at the migration-wide parity gate.*
- **Risks / failure modes**: forgetting the gateway carve-out ships a 404-from-indexer in prod —
  covered by the validated matcher change + the parity-gate routing smoke.

## Phase 5: `appFromOfficialStore` routing (spec FR-005) — done 2026-07-30
- **Goal**: non-store installs route to PAYMENT_REQUIRED, completing the spec's decision table.
- **Scope checklist**:
  - [x] (2026-07-30) Additive optional claim decode in `crates/jwt-verify`
        (`VerifiedClaims.app_from_official_store: Option<bool>`, wire name
        `appFromOfficialStore`) + surfaced on `http_common::AuthSubject`. Additive and
        optional — every sibling compiles unchanged (workspace `--all-targets` clean); absent
        claim decodes as `None` (unit-pinned in jwt-verify alongside a decode test for the set
        claims).
  - [x] (2026-07-30) Claim-path gate (`register.rs`, after the voucher branch, **before**
        DeviceCheck — spec FR-005 skips the device-identity check): `Some(false)` →
        `payment_required(...)`; `None` → pass while attestation soft mode lasts (hard-flip
        revisit note added to the attestation plan's Phase 5 checklist).
  - [x] (2026-07-30) Only consulted when the payment lane is on (a non-store user must receive
        a payable outcome, never a dead end); lane off keeps today's wire untouched.
- **✅ Demo (passed 2026-07-30, scratch Postgres in Docker + live Paseo Next v2 RPC —
  `tests/payment_http_live.rs`)**: over the production router with no DeviceCheck and no hard
  mode, an `appFromOfficialStore: false` Android token → `200 {registrationOutcome:
  "PAYMENT_REQUIRED", paymentAddress, …}`; an `appFromOfficialStore: true` token → the normal
  `202` reservation. Workspace gate green (51 suites — all siblings).
- **Risks / failure modes**: cross-crate change — kept additive-only; coordinate with any
  in-flight attestation work touching `jwt-verify`.

# Contracts (guardrails)

- **Flags off ⇒ byte-identical wire.** `REGISTRATION_VOUCHERS_ENABLED=false` +
  `PAYMENT_LANE_ENABLED=false` must keep all current fixtures green unmodified: the frozen 202
  three-key body, the bare `200 {"registrationOutcome":"PAYMENT_REQUIRED"}`, the queue fields'
  behavior under `QUEUE_ENABLED`, and the unknown-body-field tolerance for `lifetimePoUDVoucher`.
- Every lane exits through the `username_reservations` outbox; the writer path from `RESERVED` on
  is untouched; nothing but the existing writer ever submits extrinsics (the watcher is
  read-only on chain).
- Voucher plaintext is never persisted, logged, or re-derivable — hash-only at rest; mint prints
  once. No HTTP mint surface.
- One active payment request per subject; a `CONFIRMED` request is never deleted (money was
  observed — the row is the audit record).
- All inside `device-attestation` — no new crate/service (architecture guardrail); the new bins
  (`voucher-mint`) stay in the same crate like the existing three.
- New fixtures follow the classification discipline in `legacy_wire.rs` (every fixture in exactly
  one list, count assertion bumped); OpenAPI artifacts regenerated in the same change.

# Key decisions (decide early)

- **Additive outcome now, envelope flip later** — decided (matches attestation deviation #7 and
  the clients' hard-decoded 202): INSTANT/QUEUED are additive fields on the 202; only
  PAYMENT_REQUIRED uses the 200 envelope (as today, enriched). The universal
  `200 {registrationOutcome}` ships as a coordinated polish round.
- **Payment addresses = threshold-1 multisigs of the cold master + a keyless per-subject dummy**
  (`multi_account_id(sort([master, blake2_256(TAG ‖ subject)]), 1)`; revised 2026-08-03 in
  review — originally Utility derivative accounts of the master, which capped lifetime paid
  registrations at the u16 index space and needed an allocation sequence): no server secrets, the
  dummy is a hash output no key exists for so the master alone controls each deposit, unbounded
  address space, one deterministic address per subject (nothing stored beyond the subject),
  sweepable offline with `Multisig.as_multi_threshold_1`. **Accepted divergence recorded with the
  decision**: per-subject (not per-request) addresses under cumulative-balance detection mean any
  balance left at the address — an unswept earlier payment, a late deposit on an EXPIRED quote,
  FAILED_CONFLICT support money — counts toward the subject's next quote (pay once, register
  twice, bounded by sweep cadence), and anyone can precompute a subject's address (pre-funding
  confirms a quote at first watch). Mitigation: operational sweep cadence (Polish round 2
  runbook); fallback if abused: per-quote balance baselines (delta detection) or restoring a
  per-request salt in the dummy. Rejected along the way: per-request sr25519 derivation from a
  hot SURI (custody problem, threat model 6.4); a multisig with the *payer* as co-signatory
  (threshold 1 lets the payer withdraw the deposit after registration; threshold 2 strands funds
  without payer cooperation).
- **Detection = cumulative balance polling, not exact-transfer matching** — decided divergence
  from the spec's "exact amount" wording; recorded in Phase 3.
- **Watcher lives in `device-attestation-chain-writer`** — the mandatory single-instance process; the
  optional-at-runtime queue advancer must not become a payment dependency.
- **`payment-status` stays on the spec path** (`/api/v1/usernames/payment-status`) with a
  one-line gateway carve-out, rather than moving to `/api/v1/registration/…` — the spec is what
  clients will be built against; the Caddyfile is ours.
- **Voucher minting is a CLI, not a route** — the voucher pool is a Sybil-relevant secret store
  (threat model 7.2.1); the smallest attack surface wins until a real admin-auth story exists.
- **`amountRequired` unit** — open (spec text says `"<DOT>"`, legacy unmounted code used a
  planck string). No client ships against either; settle with the client team before Phase 2
  freezes the wire.
- **DOT-only payments** — the spec legend lists DOT/USDT/USDC for both top-up and
  pay-to-create; the shipped queue already reads only the People Chain DOT free balance, and
  this slice's detection does the same. Multi-asset (Asset Hub USDT/USDC) is deferred (see
  Later) — a product call, since it changes the watcher, the quote shape, and the chain surface.

# Testing

- Decision-table unit/property tests on the pure `eligibility` module (port the legacy property
  suite's invariants: voucher precedence, reject-on-spent, lane totality).
- Fixture replay: the frozen 55-fixture suite unchanged IS the flags-off regression pin (plus
  the parked DeviceCheck fixture and the payment-status mount test). The new surfaces have no
  legacy to capture, so no new fixtures were cut — their wire shapes are asserted by the live
  HTTP tests over the production router (`voucher_http_live.rs`, `payment_http_live.rs`)
  instead; captured fixtures for them arrive with the envelope-flip polish round, when the
  shapes freeze.
- Live-Postgres tests (`queue_live.rs` pattern, `just test-live`): voucher redeem atomicity under
  concurrent claims; payment confirm → reservation insert transaction; expiry sweep.
- Per-phase live demo against the dev/compose stack (Paseo Next v2 for chain-touching demos), as
  in the sibling plans.
- `just check` (includes OpenAPI sync gate) green per phase.

# Polish rounds (after MVP)

## Polish round 1: the envelope flip (client-coordinated)
- Universal `200 {registrationOutcome: INSTANT | QUEUED | PAYMENT_REQUIRED}` claim envelope
  behind a flag, shipped together with attestation Phase 5 and client releases; retire the
  parked DeviceCheck fixture classification; update `docs/architecture.md` remaining-scope
  item 2.
- ✅ Check-in demo: updated app drives claim → payment → registration against the new envelope
  end-to-end.

## Polish round 2: observability + ops
- Metrics/alerts: pending-payment depth, confirmations, expiries, voucher burn rate; sweep
  runbook for the cold master (`as_multi_threshold_1` sweeps, dummies re-derived from each
  row's subject; cadence is load-bearing — see the Key-decisions divergence); `FAILED_CONFLICT` support
  queue visibility.
- ✅ Check-in demo: Grafana panel shows a demo payment flowing through; runbook walk-through
  sweeps a test deposit.

# Later / Deferred

- **Android PoUD identifiers** — revisit only when an identifier passes all three properties
  (bound / stable / unique; see Non-goals) plus an explicit product call on Android uniqueness.
  Until then Android free claims ride the queue and the anti-Sybil bound rests on attestation
  genuineness + queue economics. The schema seam is the eligibility decision's `device` input.
- **Auth voucher rebuild** — separate slice per the attestation plan (Later).
- **Exact-transfer event matching** — only if cumulative-balance detection proves abusable in
  practice.
- **Refund / double-payment support tooling** — trigger: first real support case; `CONFIRMED`
  and `EXPIRED` rows carry everything needed.
- **Admin HTTP mint + voucher audit surface** — trigger: a real admin-auth story for V2;
  until then the CLI + `minted_batch` column is the audit trail (spec FR-010 is partially
  deferred with it).
- **Payment amount floor / governance** (threat model 7.2.2: Sybil cost scales with capital) —
  product decision on `PAYMENT_AMOUNT_PLANCK`, outside this slice.
- **Multi-asset payments and top-up (USDT/USDC)** — spec legend lists them; everything shipped
  and planned is DOT-on-People-Chain. Trigger: product prioritizes stablecoin payments; needs
  Asset Hub asset-balance reads and a quote-shape change.
- **Attestation-time voucher for LineageOS-class devices** (the diagram's "TEE + Voucher"
  integrity lane) — that voucher use sits at JWT issuance, owned by the attestation plan's
  auth-voucher follow-up, not this slice; when both exist they should share the voucher store
  or deliberately not (decide there).
- **QR voucher distribution risk** (threat model M2: photographable) — operational guidance for
  event distribution, not code.
