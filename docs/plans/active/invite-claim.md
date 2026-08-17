> Stage: **active**. Keep this plan current while working: when a scope item is finished, check its
> box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is
> the source of truth, not memory.

# Invite tickets — synchronous invitation-credential claim service

The shipping iOS/Android apps obtain Game / ProofOfInk credentials via
`POST /api/v1/invitation-ticket/claim` (iOS `InvitationApi.swift`, Android
`InvitationTicketNetworkApi.kt`). This is the **app-compatibility priority** among the remaining
slices (see the 2026-07-22 correction in the dim-tickets plan: `/api/v1/dim-ticket` has no known
client consumer). This slice ports the claim route and the ticket-pool maintainer that feeds it.

# Goals

- Preserve the legacy `POST /api/v1/invitation-ticket/claim` behavior — synchronous signature
  response over a pool of backend-generated sr25519 keypairs — in an independently deployable Rust
  service with its own Postgres.
- Keep the pool stocked: a single-instance maintainer generates keypairs, registers them on-chain
  (`set_invite_ticket` as the inviter), and inserts them as claimable only after finalization.
- Isolate the inviter signing key, the private-key-bearing ticket table, and game-chain failures
  from login, attestation, and username registration.

# Non-goals

- `/api/v1/dim-ticket` — that is the separate dim-tickets slice (user-submitted SS58, status
  polling). This service is invite tickets only: backend-generated keypairs, synchronous claim.
- Any change to the legacy wire contract — path (singular `invitation-ticket`), payloads, status
  codes, and quirks are frozen.
- Per-user claim quotas. Legacy has none — an authenticated subject may claim repeatedly; only the
  `authActions` rate bucket throttles. Deliberate parity; a quota is future hardening.
- Encryption-at-rest for ticket private keys beyond what the legacy does (plaintext column in the
  service-owned DB). Hardening slice may revisit; parity first.
- Gateway implementation, data cutover execution, and legacy shutdown (separate plans). Note for
  the cutover plan: legacy `invitation_tickets` rows contain **private keys**; cutover either
  migrates rows into this service's DB or drains the legacy pool while this pool pre-fills.

# Design principles

- User journey drives order: the claim API ships first (pool seeded by hand for the demo), the
  maintainer second.
- Verify-only JWT: gate on identity-service's Ed25519 JWKS via the shared `crates/jwt-verify`;
  never mint tokens, never call identity-service synchronously, never read its tables.
- Per-service persistence: this service owns the `invite_tickets` table in its **own** Postgres.
  Pools are keyed `(dim, network)`; the atomic claim UPDATE is the only concurrency guard.
- One signer = one nonce lane = one submitting instance. The maintainer is the only
  chain-submitting bin and never scales past one.
- `chain-types` is the only place chain types live; connection + signing come from
  `crates/chain-client`.

# User journey

1. The app, holding an identity-service JWT, POSTs `/api/v1/invitation-ticket/claim` with
   `{who, dim}` (`who` = user SS58, `dim` = `Game` | `ProofOfInk`).
2. The service atomically claims the oldest `available` ticket in the `(dim, network)` pool, signs
   the user's decoded account id with the ticket's private key, and returns the frozen 200 body
   (ticket public key, signature, inviter, timestamps, `remaining`).
3. The app uses the ticket keypair signature to complete the Game / ProofOfInk flow on-chain.
4. Independently, the maintainer keeps the pool at target: generate keypair →
   `set_invite_ticket(pubkey)` as inviter → wait finalization → insert `available`; failed
   registrations discard the keypair (no failed/retrying states — two states only).

# Frozen wire contract (ground truth: legacy code, not its docs)

Legacy sources: `apps/identity-backend/src/routes/v1/invitation-ticket.{routes,schema}.ts`,
`src/features/dim/claim-invitation-ticket.shell.ts`, `invitation-ticket-pool.shell.ts`,
`packages/lib/db/src/schema.ts` (`invitation_tickets`).

- `POST /api/v1/invitation-ticket/claim` — path segment is **singular**. Body
  `{who: <SS58, validated by decode>, dim: "Game" | "ProofOfInk"}`.
- 200: `{publicKey: 0x-hex, inviter: SS58, dim, network: "westend2"|"polkadot"|"paseo",
  claimedBy: SS58, createdAt: ISO, claimedAt: ISO, signature: 0x-hex, remaining: int}`.
- 422 `{"error": "Pool exhausted"}` — pre-check count of `available` rows in `(dim, network)` is 0.
- 409 `{"error": "Ticket race lost"}` — pre-check saw rows but the claim transaction found none
  (`FOR UPDATE SKIP LOCKED` lost the race). Order matters: pre-check 0 → 422, race → 409.
- 400 — RFC 9457 Problem Details with `errors` (schema validation; capture the exact legacy body).
- 401 `{"error": ...}` — missing/invalid JWT.
- **Signature semantics**: sr25519 signature by the **ticket** private key over the raw 32-byte
  account id decoded from `who` — NOT over the UTF-8 address string. (The legacy feature
  `ARCHITECTURE.md` §8.3 says utf8 bytes; the code — `ss58Decode(cmd.who)` then
  `keypair.sign(whoBytes)` — is the truth. Its state names are stale too: the DB enum is
  `available | claimed`.)
- FIFO: oldest `created_at` wins. `remaining` = count of `available` in the pool **after** the
  claim. No dedup, no per-user quota, no `who != inviter` check.
- Rate limit: the `authActions` bucket keyed by the authenticated JWT subject — no raw-IP keying.

# Foundations / Already shipped

- **identity-service patterns to copy**: axum + tower-http stack, fail-fast `Config::from_env`,
  auto-run sqlx migrations, utoipa OpenAPI generation (`just openapi`), compose wiring.
- **On `anp/game-invite` (unmerged as of 2026-07-22)**: `crates/jwt-verify` (Ed25519
  JWKS/public-key verification), `crates/chain-client` (reconnecting connect + `WriterSigner`,
  proxy support), and the `chain-types` metadata refresh that adds the `Game` / `ProofOfInk`
  pallets (main's metadata is still trimmed to
  `System,Balances,Utility,Proxy,People,PeopleLite,Resources`). This slice must not rebuild any of
  these — Phase 0 lands them on main.
- **Legacy tunables** (pool maintainer parity): interval 6s, batch up to 100 with adaptive halving
  on failure (batch-backoff policy), pool target 10 000, submission timeout 60s, max 5 retries,
  exponential 1s→60s. Insert uses `ON CONFLICT (public_key) DO NOTHING`; optional proxy signing.

# MVP phases (ship-shaped, demoable)

## Phase 0: Shared foundations on main

- **Goal**: `jwt-verify`, `chain-client`, and the Game/ProofOfInk chain surface exist on the branch
  this slice builds on.
- **Scope checklist**:
  - [x] (2026-07-22) `crates/jwt-verify`, `crates/chain-client`, and the `chain-types` metadata
        refresh materialized onto `anp/claim` from the dim-tickets branch, unmodified —
        identity-service deliberately untouched (it keeps its local copies until the dim-tickets
        branch merges and dedupes). Landing on **main** happens through the PR merge order:
        dim-tickets first or this branch first, either carries them.
  - [x] (2026-07-22) identity-service and username-indexer build and pass against the refreshed
        metadata (full workspace gate green: fmt + clippy `-D warnings` + 70 tests).
- **✅ Demo**: `just check` green on main with `Game.set_invite_ticket` /
  `ProofOfInk.set_invite_ticket` constructible from `chain-types`.
- **Risks**: metadata refresh bumps the typed surface for the whole workspace; coordinate with any
  in-flight branch.

## Phase 1: Claim API

- **Goal**: the frozen claim route works end-to-end against a hand-seeded pool.
- **Scope checklist**:
  - [x] (2026-07-22) New service crate `invite-tickets` with bin `invite-tickets-api`
        (`invite-tickets-pool` is declared in Phase 2 when it lands — no stub committed).
  - [x] (2026-07-22) Own Postgres + migration creating `invite_tickets` (bytea keys, CHECK
        constraints on dim/network/state, `(state, dim, network)` index; auto-run on boot).
  - [x] (2026-07-22) Route gated on `crates/jwt-verify`; the three 401 problem-details variants replayed in `tests/wire.rs`.
  - [x] (2026-07-22) Claim transaction exactly as specified (`tickets::claim_oldest`; bounded
        3× retry on transient DB errors only). Composition-tested against live Postgres
        (`tests/claim_live_pg.rs`, `--ignored`): FIFO winner, frozen 200 body, verifying
        signature, concurrent-claim winner/loser semantics, deterministic 409 via a held lock.
        Note learned there: the concurrent loser is legitimately 409 **or** 422 depending on
        whether the winner committed before the loser's pre-check — legacy behaves identically.
  - [x] (2026-07-22) sr25519 signing over the decoded 32-byte account id (schnorrkel; 32-byte
        seed and 64-byte legacy expanded secrets both load). Cross-checked in `sign.rs` tests:
        key derivation and signatures interoperate with `subxt-signer` (same public key, both
        directions verify), and signing the UTF-8 address instead provably fails verification.
        Still open (folded into the golden-fixtures item): verifying a **captured legacy**
        signature fixture.
  - [x] (2026-07-22) Per-subject fixed-window rate limit (429 + `Retry-After`, after validation, before DB — replayed in `tests/wire.rs`); no raw-IP keying.
  - [x] (2026-07-22) Fail-fast config (`DATABASE_URL`, `PEOPLE_NETWORK`, `JWT_JWKS_JSON` /
        `JWT_ED25519_PUBLIC_KEY`; redacting `Debug`). No RPC in the api bin; documented in
        `.env.example`.
  - [x] (2026-07-22) Golden legacy fixtures captured from the REAL legacy route (harness archived
        in `tests/fixtures/capture/`, provenance in `tests/fixtures/README.md`; legacy commit
        `c8369d9`): 22 request/response pairs — 401 trio, full 400 matrix, malformed JSON,
        missing-content-type 500, 200 both DIMs, 409, 422, defect 500, three 404s.
        `tests/legacy_wire.rs` replays 17 offline pairs through the production router and the 5
        DB-dependent pairs against the frozen response builders. Every previously transcribed
        expectation was confirmed by the capture, including the non-aborting empty-`who` double
        error and the SS58-refine-is-a-400 behavior.
  - [x] (2026-07-22) Dev seed path: `cargo run -p invite-tickets --example seed_pool -- Game
        paseo 10` (dev-only cargo example; inserts unregistered tickets, loudly documented as
        never-for-real-deployments).
  - [x] (2026-07-22) `#[utoipa::path]` on the claim handler, doc-only request/response schemas, crate registered in `apidoc-gen` (tags now merge across sibling docs), artifacts regenerated.
- **✅ Demo (passed 2026-07-22, scratch Postgres in Docker)**: `invite-tickets-api` booted with
  fail-fast config + auto-migrations; a minted EdDSA JWT claimed a seeded ticket over real HTTP
  (frozen 200 body, signature verified over the decoded account id); no-auth → 401 problem
  details; empty pool → 422 `Pool exhausted`; bad SS58 → 400 problem details. Concurrency
  semantics pinned by the live-PG suite (one winner; loser 409/422 by interleaving; deterministic
  409 under a held lock). Golden-fixture byte-parity remains gated on the capture item above.
- **Risks / failure modes**:
  - Signing-context mismatch (sr25519 is context-bound) — caught by the fixture-verification item.
  - Race-vs-exhausted status mapping diverging from legacy under concurrency — pin with a
    two-client integration test, not just unit tests.

## Phase 2: Pool maintainer

- **Goal**: the pool refills itself on-chain; claim never blocks on the chain.
- **Scope checklist**:
  - [x] (2026-07-22) `invite-tickets-pool` bin: loop with legacy tunables (6s interval, target
        10 000, batch ≤ 100 AIMD — grow-by-one on success, halve on failure — 60s finalization
        timeout), all env-overridable (`POOL_*`). Note: legacy's per-item retry budget does not
        apply here — failed items are discarded and regenerated (the two-state invariant), so
        "retry" is simply the next tick.
  - [x] (2026-07-22) Per `(dim, network)` pool: count → generate → `Utility.force_batch` of
        `set_invite_ticket` (WriterSigner; optional `Proxy.proxy` wrap, force_proxy_type Any as
        legacy) → parse ordered `ItemCompleted`/`ItemFailed` → insert only accepted tickets
        (`ON CONFLICT DO NOTHING`) → discard the rest. Two states only. No DB transaction is held
        across chain waits.
  - [x] (2026-07-22) Single-instance invariant: compose `replicas: 1` + a per-tick Postgres
        advisory lock (skip-if-held), so a deploy overlap cannot double-submit.
  - [x] (2026-08-04) **Inviter nonce-lane decision (blocking, cross-slice)**: resolved as
        **distinct inviter accounts per submitting service**. Compose interpolates the two
        signers from disjoint host-variable namespaces — `INVITER_*` (dim-tickets-writer, dev
        default `//Alice`) vs `INVITE_INVITER_*` (this bin, dev default `//Bob`) — so the two
        bins cannot share a signing account by default, and `docs/deploy.md` ("Nonce lanes")
        mandates distinct accounts in production (e.g. separate proxy delegates of one cold
        inviter primary). This makes the invariant independent of whether `dim-tickets-writer`
        ever deploys (it currently has no client consumer). Recorded in the dim-tickets plan's
        Key decisions too.
  - [x] (2026-07-22) Fail-fast config (`MaintainerConfig::from_env`): required
        `DATABASE_URL`/`PEOPLE_RPC_URL`/`PEOPLE_NETWORK`/`INVITER_ADDRESS`/`INVITER_SIGNER_SURI`,
        optional `POOL_DIMS`, bounded tunables; redacting `Debug`. (Proxy signing was configured by
        `INVITER_PROXY_FOR` and validated against `INVITER_ADDRESS` until #46 made it derived.)
  - [x] (2026-07-22) Structured logs: pool size per tick, per-batch submitted/registered/failed
        with block hash, throttle transitions. (Proper metrics/alerts remain hardening-slice
        scope; the runbook documents the log-based alert threshold.)
- **✅ Demo (partially passed 2026-07-22, live Paseo + scratch Postgres)**: the maintainer
  connected to the public Paseo People RPC, signed and submitted real `force_batch` transactions,
  and a batch **finalized on-chain** (block `0xd8cf…`) with per-item events parsed —
  `submitted=1 registered=0 failed=1` (`//Alice` holds no invite quota) and, correctly, zero rows
  inserted. Whole-batch failures (nonce conflicts with a prior run's in-flight tx — the deploy
  overlap scenario) throttled the batch 3→1 and the loop recovered. **Remaining to pass in
  full**: the refill-to-target happy path (`registered > 0`), gated on a funded inviter with
  Game/ProofOfInk quota — the same standing gap as the dim-tickets slice.
- **Risks / failure modes**:
  - Nonce races if the invariant above is unresolved — this is the slice's main external risk.
  - Finalization takes minutes; the maintainer must never hold a DB transaction across chain waits.
  - Pool depletion under claim bursts — the gauge + alert (Phase 3) is the operator's early signal.

## Phase 3: Deployment and parity gate

- **Goal**: independently deployable, observable, and provably compatible.
- **Scope checklist**:
  - [x] (2026-07-22) Compose/deploy entries: `invite-tickets-postgres` (:5435),
        `invite-tickets-api` (:8083; 5434/8082 are claimed by the dim-tickets branch), and
        `invite-tickets-pool` (`replicas: 1`); the inviter SURI reaches only the pool service's
        environment; api `/readyz` gates on its DB only (no chain). Dockerfile builds both bins;
        deploy.md carries the Caddy route + secret boundary; runbook carries the failure modes.
  - [x] (2026-08-04) Prove identity-service, username-indexer, and the others stay healthy with
        this service stopped, and vice versa. Ran against the full compose stack: with
        `invite-tickets-{api,pool,postgres}` stopped, all five sibling APIs (identity-api,
        username-indexer, dim-tickets-api, turn-api, notify-relay) answered `/readyz` 200 on
        9 polls over 45s (0 failures) and the workers stayed up; with ALL siblings stopped
        (identity-api, chain-writer, registration-queue, username-indexer, dim-tickets-api/writer,
        turn-api, notify-relay), invite-tickets-api answered `/readyz` 200 on 6 polls over 30s and
        served a real authenticated claim (frozen 200 body) from its own DB — JWT verified
        offline via the dev public key, no identity-service needed.
  - [x] (2026-08-04) End-to-end smoke vs the legacy backend: contract-equal responses proven
        live. The REAL legacy backend (repo `main`, superset of capture commit `c8369d9`; bun,
        `TZ=UTC`, `JWT_AUTH_ENFORCED=true`, throwaway migrated Postgres) and `invite-tickets-api`
        (compose) were seeded with **identical** ticket pools (same sr25519 keypairs, same
        `created_at`s), then the golden matrix was replayed against both live servers (harness
        archived at `tests/fixtures/capture/smoke-invite-claim.live.mjs`): all 17 static pairs
        (401 trio, 400 matrix, malformed JSON, missing-content-type 500, three 404s) byte-equal
        across legacy-live / rust-live / fixture; 3 claims (Game×2, ProofOfInk×1) picked the same
        FIFO ticket on both sides, signatures sr25519-verified over the decoded account id, and
        normalized bodies matched; 422 after drain byte-equal; end DB states identical. "Same
        JWT" is per-backend by design: legacy verifies HS256, this service Ed25519 — same
        subject, both minted for the run (the algorithm split is the recorded fixtures
        approximation). One divergence found and accepted: **legacy's `remaining` has a
        pre-commit count race** (its post-claim COUNT can run before the claim transaction
        commits — `resume()` fires inside the tx callback — so it reports +1 on a fresh
        connection's read). This service counts after commit, which is the transcribed contract
        ("`remaining` = count of `available` **after** the claim"); the smoke tolerates
        legacy at expected/expected+1 and pins rust to the exact value.
  - [x] (2026-07-22) Pool-size alert threshold documented in the runbook (sustained
        `available < ~10%` of target).
  - [x] (2026-07-22) `docs/architecture.md` updated (service tables, crate statuses, the
        invite-tickets invariants block); `.env.example` documents the new service.
- **✅ Demo (parity half passed 2026-08-04)**: `docker compose up` serves a real app-shaped claim
  flow (authenticated claim over real HTTP from the compose stack, isolation proven both ways)
  and the parity fixtures pass against both live backends (see the smoke item above). The
  pool-refilling half remains gated on a funded inviter with Game/ProofOfInk quota — the same
  standing gap as Phase 2's `registered > 0` demo; everything else in this phase is done.
- **Risks**: release-device validation is a separate later slice (roadmap item); do not gate this
  phase on physical devices.

# Post-merge cleanup (done)

The axum scaffolding in `invite-tickets` deliberately mirrors the dim-tickets crate, which lives on
a sibling branch and could not be imported: `http/extract.rs` is byte-identical, and the rate
limiter, the shared error dialects (401 trio / 429 / 400 problem details / Hono text 400 / opaque
500), health shape, and the config helpers are near-clones. Once both service crates are on main
that is the second-consumer condition met — extract the shared HTTP primitives into a small shared
crate (the same rule that produced `jwt-verify`) instead of letting a third service clone them
again. Neither service crate should import the other.

**✅ Done (2026-07-22)**: extracted as `crates/http-common` — `problem` (the frozen dialects),
`zod` (v4 message fragments), `auth` (`AuthSubject` over a `HasJwtVerifier` state trait),
`rate_limit`, `health` (closure-based readiness probes), `layers` (the tower-http stack), and
`config` (fail-fast env helpers + JWT verifier loading). Both service crates now consume it;
neither imports the other; both golden-fixture replays stayed green through the swap. The
dim-tickets golden capture (done the same day) folded the corrected dialects (Oops-500, Hono text
400/404) into the shared crate, so a third service gets the captured behavior for free.
