> Stage: **active**. Keep this plan current while working: when a scope item is finished, check its
> box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is
> the source of truth, not memory.

# Proof-of-compute gate — puzzle issuance + public-read gate

Karim's V2 proposal lists a **proof-of-compute gate** as a required supporting capability of the
username indexer ("client requests puzzle, solves it, presents solution", stateless verification).
It is the last unbuilt item on that keep-list, and the `/api/v1/poc*` block in `gateway/Caddyfile`
(lines 63–66) is still on the cutover lane marked "unowned — needs an in/out ruling". This plan
rules it **in** and ports it.

Provenance of the frozen contract: legacy `paritytech/identity-backend` —
`apps/identity-backend/src/routes/v1/poc/issue-poc.route.ts`,
`src/proof-of-compute/{proof-of-compute.schema,issue-proof-of-compute.executor,verify-proof-of-compute.executor,spent-puzzles.store}.ts`,
`src/middleware/proof-of-compute.middleware.ts`, `src/lib/problem-details.ts`, config at
`src/config.ts:788-935`. Legacy has **no verify endpoint** — the solution travels in a request
header on the gated route.

Two facts that shape the plan:

- **The gate exists for the non-mobile, JWT-less callers** (the Desktop/host path) reading public
  search — which is why it sits on the unauthenticated read as an *alternative* to a bearer token,
  never as an extra hurdle on top of one. Client evidence (2026-07-29):
  - **Both mobile apps already authenticate search**, so they are unaffected by the flip. iOS uses
    `createJWTAuthorizedRequestWrapper` for `GET api/v1/usernames/search?prefix=…`
    (`Common/Username/Networking/UsernameOperationFactory.swift:166-175`, bearer applied by
    `BearerTokenRequestModifier.swift:10-14`); Android's search method carries
    `@CallWithBearerToken` on `@GET("api/v1/usernames/search?status=ASSIGNED")`
    (`feature_usernames_impl/.../UsernameApi.kt:30-34`, interceptor
    `BearerTokenInterceptor.kt:13-43`). Caveat: Android's `nightly` build type sets
    `DISABLE_AUTH=true` (`tools/jwt-auth/impl/build.gradle.kts:8-16`), so nightly builds would need
    a puzzle.
  - **Desktop is the anonymous caller.** It searches through `@novasamatech/host-chat`
    `accountService.search()` with `Accept: application/json` and **no** `Authorization` header
    (`triangle-js-sdks/packages/host-chat/src/accountService.ts:64-86`; Desktop wires
    `identityEndpoint` at `src/domains/chat/p2p/peer/gateway.ts:80-99`). Separately it is still on
    the **sunset bare-path** `GET /api/v1/usernames`, which this repo already 404s — that SDK fix is
    a prerequisite tracked by the usernames/cutover work, not by this plan.
- **No client solves PoC today.** Scans for `poc` / `Proof-Of-Compute` / `proofOfCompute` /
  `poc/issue` find nothing in `polkadot-app-ios-v2`, `polkadot-app-android-v2`, `polkadot-desktop`
  (verified against remote `main` via GitHub code search, 0 hits), `truhost`, `useragent-kit`, or
  `dotli-community`. Legacy's only consumers are its own test/load tooling, its e2e suite is
  `describe.skip`, and legacy never installed the middleware on any route
  (`routes/v1/username/mod.ts:19-34` wires CORS/auth/rate-limits only —
  `username-search.routes.ts:42-72` merely *documents* the header). So this ships **off by default**;
  because the gate is either-or, turning it on costs the mobile apps nothing and only obliges the
  anonymous host/Desktop path.
- **Legacy replay protection is per-process in-memory** (`spent-puzzles.store.ts`), i.e. broken
  across replicas. V2 fixes that with a Postgres consume in the owning service, which already has
  its own database.

# Goals

- `POST /api/v1/poc/issue` is served by the Rust stack, byte-compatible with legacy: `201`
  `{sessionId, timestamp, difficulty, checksum}`, HMAC-SHA256 checksum over the legacy preimage,
  stateless issuance (no DB row).
- **The gate is either-or: a valid JWT satisfies it, otherwise a solved puzzle is required.** A
  solved puzzle presented as `Proof-Of-Compute: base64(sessionId:timestamp:difficulty:counter:checksum)`
  admits an anonymous caller to `GET /api/v1/usernames/search`, with legacy's exact `400`
  bad-request and `402` payment-required problem-details bodies and detail strings. Bearer callers
  skip the puzzle entirely — this is legacy's stated intent (`proof-of-compute/AGENTS.md:9-11`:
  "bearer-authenticated callers were intended to bypass by route composition") and it is what keeps
  the shipping mobile apps working untouched.
- Verification is stateless except for **one-shot consume**: a session id can be spent exactly once
  across all replicas.
- `POC_ENABLED=false` (the default) behaves exactly like today: no `/issue` route (frozen Hono
  `404`), gate passes through, search unchanged.
- The gate is demonstrably solvable end-to-end by a first-party solver against the live test
  deployment, so a client team can integrate against a proven contract.

# Non-goals

- Gating any other surface (`/auth/*`, `/usernames` write, availability, DIM/invite/turn/notify).
  Karim scopes PoC to the unauthenticated indexer read; authenticated routes are JWT + rate-limited.
- A `/poc/verify` endpoint. Legacy has none; the header is the contract.
- Flipping `POC_ENABLED=true` in production, or any client-side (iOS/Android) implementation. The
  enablement ruling is Phase 5 output, not this plan's deliverable.
- Changing the search response shape, cursor semantics, or the existing per-IP rate limiter.
- Replacing edge protection (Cloudflare-class rules stay the outer defence).
- Reworking the shared-egress vs general rate-limit profiles from Karim's doc (separate slice).

# Design principles

- **User journey drives order**: issue → solve → gated read → deployed → ruled on.
- **Frozen wire**: preimages, encodings, status codes, and error strings are ported byte-for-byte
  from the legacy source; a known-answer test pins them.
- **The owner is `username-indexer`** — the only gated surface lives there, it already owns the
  public-read anti-abuse layer (`crates/username-indexer/src/http/middleware.rs`, whose module doc
  already says "there is no JWT and no proof-of-compute here") and it has its own Postgres for the
  consume. No cross-service secret sharing, no synchronous call to identity-service.
- **Reuse before rebuild**: dialect bodies go in `crates/http-common/src/problem.rs` (where the
  frozen dialects live), config uses the crate's existing `required`/`parse_var` fail-fast helpers,
  the route sits inside the existing `layers::standard_layers` + `hono_not_found` router, and
  migrations follow the auto-run `sqlx::migrate!` convention (runtime-checked queries, no `query!`).
- **Off by default, security in the MVP**: the flag defaults false, but when it is on, checksum,
  expiry, work, and one-shot consume are all enforced — no "add replay protection later".

# User journey

1. A **bearer-authenticated** client (both shipping mobile apps) calls
   `GET /api/v1/usernames/search?...` exactly as today and is served — the puzzle never enters its
   flow.
2. An **anonymous** client (Desktop / host path) calls `POST /api/v1/poc/issue` and receives a puzzle.
3. It mines `counter` until `sha256(sessionId || timestamp || counter)` has at least `difficulty`
   leading zero bits.
4. It calls `GET /api/v1/usernames/search?...` with the solved `Proof-Of-Compute` header.
5. The indexer verifies JWT-or-puzzle: no bearer ⇒ checksum → expiry → work → consume, then serves
   the search results.
6. A second anonymous request reusing the same puzzle is rejected `402`; the client issues a new one.

# Foundations / Already shipped (✅)

## `username-indexer` public read surface
- What exists: `GET /api/v1/usernames/search` on the frozen legacy dialect, per-IP fixed-window
  rate limiter (`http/middleware.rs`, `SEARCH_RATE_LIMIT` / `SEARCH_RATE_LIMIT_WINDOW_SECS`),
  `hono_not_found` fallback for unmatched paths and methods, `layers::standard_layers`.
- ✅ Demo: `cargo test -p username-indexer` (fixture replay suite) passes.
- Gaps: no PoC issuance route, no header extractor, no gate.

## Frozen error dialects
- What exists: `crates/http-common/src/problem.rs` renders RFC 9457 problem details against the
  `https://problems-registry.smartbear.com` registry (401 trio, 429, zod 400 variants, Hono text
  bodies, opaque 500).
- ✅ Demo: existing `legacy_wire` fixture suites across services pass.
- Gaps: no `payment-required` (402) renderer and no plain `bad-request` (400) renderer — legacy's
  PoC middleware uses both.

## Own-Postgres + auto-migrations
- What exists: `crates/username-indexer/migrations/*` auto-applied by `sqlx::migrate!` in
  `db::connect`; advisory-lock-serialized projection writes make N replicas safe.
- ✅ Demo: `docker compose up` boots the indexer against its own Postgres.
- Gaps: no table for spent puzzles.

## Gateway ownership map
- What exists: `gateway/Caddyfile` with `@username_reads` → `:8081` and the legacy cutover lane.
- ✅ Demo: compose `gateway` on `:8000` routes search to the indexer.
- Gaps: `/api/v1/poc*` still forwards to `LEGACY_BACKEND_ORIGIN`.

# MVP phases (ship-shaped, demoable)

## Phase 1: Puzzle issuance — ✅ done 2026-07-29
- **Goal**: a client can get a legacy-identical puzzle from the Rust stack.
- **Scope checklist**:
  - [x] `crates/username-indexer/src/poc.rs` (+ `poc/` children): `SessionId`, `Puzzle`,
        `checksum_preimage` = 16 UUID bytes (hyphens stripped) `||` timestamp as u64 big-endian
        `||` difficulty as one byte; `checksum` = lowercase hex HMAC-SHA256 over that preimage.
  - [x] Secret derivation: HKDF-SHA256(ikm = `POC_HMAC_SECRET`, no salt,
        info = `identity-backend/proof-of-compute/hmac-v1`, 32 bytes) — same derivation as legacy
        `proof-of-compute.config.ts`, so a legacy-issued puzzle stays verifiable when the same IKM
        is deployed during cutover.
  - [x] Config in `username-indexer/src/config.rs` via the existing helpers: `POC_ENABLED`
        (default `false`), `POC_DIFFICULTY_BITS` (default `16`, validated `1..=32` — note legacy's
        `proof-of-compute/AGENTS.md` claim of 4 is stale vs `config.ts:901`), `POC_SESSION_TTL_SECS`
        (default `60`), `POC_CLOCK_SKEW_SECS` (default `30`), `POC_HMAC_SECRET`
        (**required only when `POC_ENABLED=true`**, fail-fast otherwise unchanged).
  - [x] Route `POST /api/v1/poc/issue` mounted **only when `POC_ENABLED`** → `201`
        `{sessionId, timestamp, difficulty, checksum}` (`timestamp` = unix **millis**);
        flag off ⇒ route absent ⇒ existing `hono_not_found`. Method fallback keeps wrong-method
        requests on the Hono 404, like the search route. Request body/query/headers are ignored
        (legacy declares no schema and reads nothing).
  - [x] Known-answer test pinning the legacy vector: secret `vector-secret`, session
        `1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed`, timestamp `1700000000000`, difficulty `4` ⇒
        checksum `c8828951fd6c123fdbf6501f111d27dd3f260839344a7370e0dd8f20e2c40482`.
  - [x] `#[utoipa::path]` + `openapi.rs` registration; `just openapi` regenerated and committed.
- **✅ Demo**: with `POC_ENABLED=true`, `curl -X POST :8081/api/v1/poc/issue` returns a `201`
  puzzle whose checksum the KAT-verified code recomputes; with the flag off the same call returns
  the frozen `404 Not Found` text body. `just check` green.
- **Risks / failure modes**:
  - Wrong UUID→bytes handling or endianness silently breaks compatibility — the KAT is the guard.
  - Making `POC_HMAC_SECRET` unconditionally required would break every existing deployment; it
    must be conditional on the flag.

## Phase 2: Verifier core + first-party solver — ✅ done 2026-07-29
- **Goal**: prove the puzzle is solvable and the verifier accepts exactly what legacy accepts.
- **Scope checklist**:
  - [x] `Solution` parse: base64 → `sessionId:timestamp:difficulty:counter:checksum`, with legacy's
        field validation (UUID; integer timestamp `0..=8_640_000_000_000_000`; difficulty `1..=32`;
        non-negative counter; checksum strictly 64 hex chars).
  - [x] Work function: `leading_zero_bits(sha256(uuid_bytes || u64be(timestamp) || u64be(counter)))`
        over the **first 4 bytes** (legacy uses `Math.clz32` on the leading big-endian u32);
        accept when `>= solution.difficulty`.
  - [x] Verification order **checksum → expiry → work** (matches legacy so the reported reason is
        identical), expiry = `now_ms - timestamp <= ttl_ms + skew_ms` with **no lower bound**
        (future timestamps are accepted — deliberate legacy parity, and harmless because the
        checksum proves we issued it).
  - [x] KATs: `leading_zero_bits(session, 1700000000000, 0) == 3` and
        `(…, 12345) == 0`; expiry boundary exact-at-limit valid / +1ms expired; forged checksum
        rejected; difficulty-below-requirement rejected.
  - [x] `crates/username-indexer/examples/poc_solve.rs` — issue → mine → print the ready-to-send
        header value (the dogfooding tool for Phases 4–5, mirroring
        `identity-service/examples/auth_smoke.rs`).
- **✅ Demo**: `cargo test -p username-indexer poc` green; `cargo run -p username-indexer
  --example poc_solve` against a locally running indexer mines a solution at the default 16 bits
  and prints a header that the verifier unit-accepts. Record the observed mine time.
- **Risks / failure modes**:
  - `clz32`-on-first-u32 vs full-digest leading-zero counting differ above 32 bits; difficulty is
    capped at 32 precisely because of this — keep the u32 reading.
  - 16 bits may be too slow/fast on target devices; the measurement here feeds the Phase 5 ruling.

## Phase 3: The gate on public search (JWT **or** puzzle) — ✅ done 2026-07-29
- **Goal**: the journey closes — an *anonymous* search request needs a fresh solved puzzle, while a
  bearer-authenticated one is served exactly as today.
- **Scope checklist**:
  - [x] `http-common::problem`: `bad_request(detail)` (400) and `payment_required(detail)` (402)
        renderers on the existing `problem()` helper, with the registry types
        `…/bad-request` / `…/payment-required` and titles `Bad Request` / `Payment Required`.
  - [x] **Verify-only JWT in `username-indexer`**: add the existing `jwt-verify` crate as a
        dependency and accept `JWT_ED25519_PUBLIC_KEY` / `JWT_JWKS_JSON` (public material only, no
        signing key). A request with a **valid** bearer bypasses the puzzle; an **invalid/expired**
        bearer is treated as anonymous and falls through to the puzzle requirement (never a 401 —
        search must not become an authenticated route).
  - [x] Gate middleware in `username-indexer/src/http/middleware.rs`, layered **inside** the
        existing per-IP rate limiter (cheap limiter first, then JWT check, then the crypto), applied
        to `GET /api/v1/usernames/search` only; pass-through when `POC_ENABLED=false`.
  - [x] Rate-limit keying follows the same either-or split: authenticated requests key on the JWT
        subject, anonymous ones stay per-IP (as today).
  - [x] Exact legacy detail strings: malformed header ⇒ `400` `The Proof-Of-Compute header is
        malformed.`; missing ⇒ `402` `Proof of compute required. Request a puzzle from POST
        /api/v1/poc/issue and present the solved proof in the Proof-Of-Compute header.`;
        checksum ⇒ `The proof checksum does not match; the puzzle was not issued by this server.`;
        expired ⇒ `The proof of compute puzzle has expired; request a new one.`;
        replay ⇒ `The proof of compute puzzle has already been used.`;
        insufficient work ⇒ `The proof of compute solution does not meet the required difficulty.`
  - [x] Migration `crates/username-indexer/migrations/20260729000000_create_spent_puzzles.sql`:
        `spent_puzzles(session_id uuid primary key, expires_at timestamptz not null)`.
  - [x] One-shot consume: `INSERT … ON CONFLICT DO NOTHING` (runtime-checked `sqlx::query`, bound
        params) after work verification; zero rows affected ⇒ the replay `402`. Retain until
        `timestamp + ttl + skew`; prune expired rows on the existing sync loop tick.
  - [x] Fixture/replay tests for all six error bodies, the happy anonymous path, **the bearer bypass
        (200 with no puzzle)**, the invalid-bearer-falls-through-to-402 case, and a live-Postgres
        replay test alongside the existing live-PG test convention.
- **✅ Demo**: `POC_ENABLED=true` locally — search with a valid JWT and **no** puzzle ⇒ `200`
  (proving both mobile apps are unaffected); anonymous with no header ⇒ `402` with the exact legacy
  body; garbage header ⇒ `400`; anonymous with the Phase 2 solver's header ⇒ `200`; replaying that
  header ⇒ `402` "already been used"; a second indexer replica rejects the replay too (cross-instance,
  unlike legacy). Flag off ⇒ every call behaves exactly as today.
- **Risks / failure modes**:
  - Layer order: putting the gate outside the rate limiter would let cheap unsolved requests burn
    CPU; putting it on the whole router would gate `/livez`/`/readyz` and break orchestration.
  - Consuming before work verification would let an attacker burn other clients' sessions.
  - Returning `401` for a bad bearer would turn a public route into an authenticated one and break
    Desktop; bad bearer must degrade to "anonymous", not reject.
  - Android's `nightly` build type ships `DISABLE_AUTH=true`
    (`tools/jwt-auth/impl/build.gradle.kts:8-16`) — nightly builds hit the anonymous lane. Call this
    out to the Android owners before any flip.

### Implementation notes (what shipped, and where it differs from the plan text)

- **Where the code lives**: `crates/username-indexer/src/poc.rs` + `poc/{puzzle,solution,store}.rs`;
  gate and limiter in `src/http/middleware.rs`; issuance route in `src/http.rs`; migration
  `migrations/20260729000000_create_spent_puzzles.sql`; solver `examples/poc_solve.rs`; tests
  `tests/poc_gate.rs` (11 offline cases) and `tests/poc_gate_live.rs` (`#[ignore]`, live Postgres).
  `http-common::problem` gained `bad_request` / `payment_required`.
- **The legacy KAT keys the HMAC with the secret bytes directly.** The vector in
  `issue-proof-of-compute.executor.ts` is *not* HKDF-derived: legacy derives once at config time and
  passes the derived key in. Both layers are now pinned separately — the checksum vector
  (`c8828951…`) against a literal key, and HKDF-SHA256 with legacy's `info` string against an
  independently computed RFC 5869 vector (`7fff0dec…`).
- **Bearer verification happens once per request.** `rate_limit` resolves the subject (it needs it
  for keying anyway) and stashes it in the request extensions; `poc_gate` reads that instead of
  verifying again, but still falls back to verifying itself so the gate is correct standalone.
- **Pruning runs as its own interval task** in the bin (period = `POC_SESSION_TTL_SECS`) rather than
  on the sync loop tick, which keeps `sync.rs` free of gate concerns.
- **Issuance is deliberately not rate limited** (legacy parity: its `/poc` router had no limiter).
  Issuance writes nothing and is one HMAC; revisit if it is ever abused.
- **Phase 4 config work landed 2026-07-29**: gateway route, compose wiring, boundary script, and
  runbook are in. The only Phase 4 item left is the live smoke on the test deployment, which needs a
  deploy and therefore explicit consent.

## Phase 4: Own the route and prove it on the test deployment
- **Goal**: `/api/v1/poc*` leaves the legacy cutover lane and the gate runs on real infrastructure.
- **Scope checklist**:
  - [x] (2026-07-29) `gateway/Caddyfile`: `/api/v1/poc*` now reverse-proxies to
        `{$USERNAME_INDEXER_UPSTREAM}` instead of importing `legacy`; `docs/plans/active/gateway.md`
        route table flipped from "unowned" to migrated. Validated with `caddy validate`.
  - [x] (2026-07-29) `.env.example`: the five `POC_*` vars documented on the username-indexer
        section, flag off by default. Still to do: `docker-compose.yml` wiring, and
        `scripts/verify_compose_boundaries.sh` updated — `scripts/verify_compose_boundaries.sh` updated — the PoC secret asserted
        **indexer-only** (must not leak into identity-api), and the indexer's current
        "no JWT material at all" assertion (`scripts/verify_compose_boundaries.sh:88-89`) relaxed to
        **public verification material only**: `JWT_ED25519_PUBLIC_KEY` / `JWT_JWKS_JSON` allowed,
        `JWT_ED25519_SECRET` still forbidden.
  - [x] (2026-07-29) `docs/architecture.md`: classification entry (`proof-of-compute →
        username-indexer`), the indexer service row, the corrected "no JWT/JWKS" HTTP note, the
        subject-or-IP rate-limit note, and the JWT-or-puzzle invariant. Landed with Phase 3 rather
        than deferred, because leaving those lines stale would have made the doc wrong.
  - [x] (2026-07-29) `docs/runbook.md`: a "Proof-of-compute gate" section (enable/disable, required
        verify-only material, smoke commands, difficulty tuning), the `POC_HMAC_SECRET` rotation
        note, the `:8081` route note, and three failure-mode rows (`402` spikes, replayed puzzles,
        `spent_puzzles` growth).
  - [ ] Deploy to `identity.dotspark.app` with `POC_ENABLED=true` **on the indexer only** and run
        the solver against the public URL through the gateway; also re-run one authenticated search
        to prove the bearer bypass on real infrastructure.
- **✅ Demo**: on `identity.dotspark.app`, `POST /api/v1/poc/issue` returns a puzzle (served by the
  indexer, not legacy), the solver's header yields a `200` search, an authenticated search with no
  puzzle yields `200`, and a replay yields the `402` — then the flag goes back off.
- **Risks / failure modes**:
  - Anonymous callers regress the moment the flag is on: the Desktop/host path and Android nightly
    builds get `402` until they mine. The shipping mobile apps do not (bearer bypass). Keep the
    on-for-the-smoke / off-after sequence and let Phase 5 gate the real flip.
  - A stale `LEGACY_BACKEND_ORIGIN` default could keep serving the legacy `/poc` route; verify the
    response comes from the Rust service (e.g. distinct build/log correlation) during the smoke.

## Phase 5: Enablement ruling + client handoff
- **Goal**: a decision, not a dangling capability — either the anonymous callers integrate or the
  flag stays off with a documented reason.
- **Scope checklist**:
  - [ ] Report the measured mine time at 8 / 12 / 16 / 20 bits from the Phase 2/4 runs, and pick the
        recommended `POC_DIFFICULTY_BITS` for the slowest expected client (desktop/host JS or Rust
        miner, not just a phone).
  - [ ] Hand the frozen contract (issue body, header format, preimages, KAT vector, error bodies) to
        the owners of the **anonymous** callers: `@novasamatech/host-chat` / `polkadot-desktop` and
        the `useragent-kit` / `truhost` host stack — noting they can satisfy the gate *either* by
        mining *or* by sending an existing bearer token, whichever is cheaper for them. The two
        shipping mobile apps need no change (verified bearer on search).
  - [ ] Flag the Android `nightly` `DISABLE_AUTH=true` build to the Android owners — it is the one
        mobile configuration that lands in the anonymous lane.
  - [ ] Decide and record: flip on for anonymous search, or keep off with rate-limit-only protection.
        Note that legacy never enforced it, so enabling is a **new** requirement for anonymous
        callers, not parity.
  - [ ] If flipping on: sequence it after the anonymous callers ship either a miner or a bearer, and
        update `docs/architecture.md` remaining-scope + this plan → `docs/plans/done/`.
- **✅ Demo**: the ruling is written down in this plan with the measurement table, and either a
  client commit references the header or the plan records the deferral trigger.
- **Risks / failure modes**:
  - Shipping the gate and never ruling leaves dead config; the plan does not move to `done/` until
    this phase closes.

# Contracts (guardrails)

- `POC_ENABLED=false` is byte-for-byte today's behavior: no `/poc` route, no header parsing, search
  and its per-IP limiter unchanged.
- Issuance stays **stateless** — no DB write at issue time; only verification consumes a row.
- Checksum preimage, work preimage, base64 header layout, `201` field names, and all six error
  bodies match the legacy source exactly (KAT + fixture replay enforce this).
- The gate applies only to `GET /api/v1/usernames/search`. Health endpoints are never gated.
- **A valid bearer token always satisfies the gate**, and an invalid one never turns search into an
  authenticated route (no `401` from this middleware — it degrades to the anonymous lane). The
  shipping mobile apps must keep working with zero client changes at any `POC_ENABLED` setting.
- `username-indexer` holds **public** JWT material only (`JWT_ED25519_PUBLIC_KEY` / `JWT_JWKS_JSON`);
  `JWT_ED25519_SECRET` stays forbidden there, and it never mints a token.
- `POC_HMAC_SECRET` lives only in `username-indexer`; the compose boundary script proves no other
  service receives it.
- No shared tables, no synchronous cross-service call: the indexer does not read identity-service
  state to verify a puzzle or a token.
- `docs/api-reference/{openapi.json,index.html}` are regenerated with `just openapi`, never hand-edited.

# Key decisions (decide early)

- **Owner service — `username-indexer` (recommended, assumed by this plan).** The alternative is
  issuing in `identity-service` and verifying in the indexer, which would require the same symmetric
  secret in two services for zero benefit. Reversing this later means moving a route and a
  migration, so confirm before Phase 1.
- **JWT-or-puzzle rather than puzzle-for-everyone.** Evidence-backed: both shipping apps already
  send a bearer on search, Desktop does not, and legacy's own notes say bearer callers were meant to
  bypass. This makes the indexer verify-only-JWT-capable — a boundary change worth acknowledging
  (public key only) but not a new secret.
- **Legacy-compatible secret derivation.** Keeping HKDF with legacy's info string lets both stacks
  verify each other's puzzles during cutover if the same IKM is configured; the alternative is a
  fresh random secret and a hard cutover boundary on `/poc`.
- **Cross-instance replay via Postgres** rather than copying legacy's in-memory store. Costs one
  insert per gated request; the alternative is a known replay hole across replicas.
- **Difficulty default 16 bits** (legacy's executable default) until Phase 5 measurement says
  otherwise.

# Testing

- Manual smoke demos per phase, as written in each ✅ Demo.
- Regression tests limited to the contracts: the issuance KAT, the work/expiry KATs, the six
  frozen error bodies, the flag-off pass-through, and a live-Postgres one-shot-consume test.
- `just check` (fmt + clippy `-D warnings` + workspace tests) green before each phase is called done.

# Polish rounds (after MVP)

## Polish round 1: operational visibility
- Counters for issued puzzles, and `402` verdicts split by reason (missing / checksum / expired /
  replay / work), so a difficulty change can be judged from data.
- ✅ Check-in demo: a difficulty change is visible as a shift in the verdict mix rather than a
  guess. (Lands with the metrics/monitoring slice — PoC only contributes its counters.)

## Polish round 2: consume-path efficiency
- Prune batching and, if measurements require it, an in-process negative cache in front of the
  consume insert.
- ✅ Check-in demo: gated-search p99 latency at the chosen difficulty unchanged under a load run.

# Later / Deferred

- **Gating other unauthenticated surfaces** — revisit if a new public read is added.
- **A `/poc/verify` endpoint** — only if a client cannot attach the header to the gated call.
- **Difficulty adaptivity (per-IP or under load)** — revisit if a real abuse event shows a fixed
  difficulty is the wrong knob.
- **Shared-egress vs general rate-limit profiles** (also from Karim's doc) — separate slice; PoC
  does not depend on it.
- **Retiring the gate entirely in favour of edge protection** — revisit if Phase 5 concludes no
  client will implement it and the per-IP limiter plus edge rules are judged sufficient.
