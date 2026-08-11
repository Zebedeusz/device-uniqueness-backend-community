# Test-coverage remediation — close the real gaps, keep the ceremony small

> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

Baseline measured 2026-08-05 (`cargo llvm-cov` 0.8.7, main, workspace suite
**plus** all live-Postgres suites merged into one profile): **63.4% line /
59.3% function** over 13,907 instrumented lines. The live tests are
load-bearing — without them the total drops to 58.9% and `payment.rs` reads
23% instead of 92% — so any coverage number quoted without them is wrong.

Per-crate lines covered (merged):

| Crate | Lines | Coverage | | Crate | Lines | Coverage |
|---|---|---|---|---|---|---|
| jwt-verify | 171 | 100% | | identity-service | 6,602 | 64.2% |
| apidoc-gen | 651 | 96.5% | | username-indexer | 1,986 | 63.0% |
| chain-client | 134 | 92.5% | | chain-types | 133 | 48.1% |
| notifications | 1,059 | 74.4% | | invite-tickets | 1,022 | 41.7% |
| turn | 368 | 71.7% | | dim-tickets | 1,316 | 40.2% |
| http-common | 465 | 71.6% | | | | |

The gaps cluster into four kinds, and they are **not** equally important:

1. **Security-critical glue with almost no tests**: `auth/token.rs` (12.5%),
   `auth/refresh.rs` (16.8%), `auth/challenge.rs` (36.7%),
   `auth/app_attest/store.rs` (0%) — token issuance/rotation and the
   sign-count monotonic guard, sitting next to well-tested attestation
   *verify* modules.
2. **Writer/outbox state machines**: `chain/outbox.rs` guarded mutations —
   including the stale-lease-returns-false safety property — are 0%-covered;
   `chain/writer.rs` (22.7%), dim `writer.rs` (19.2%), invite `pool.rs`
   (5.3%), all with decision logic still inline in I/O loops.
3. **Indexer ingest**: `bootstrap.rs` (0%), `incremental.rs` (7.7%),
   `sync.rs` (44.8%) — the write side of the indexer is untested while the
   read side (`search.rs` 97%) is golden-pinned.
4. **Pure code nobody wired up**: `identity-service/http/error.rs` (0%),
   `config.rs` env parsing (18.9%), `chain-types` builder/Noop encoders —
   plain unit-test targets.

# Goals

- Every state-machine transition that guards money, chain writes, or auth is
  pinned by a test: outbox guarded mutations (incl. stale-guard rejection),
  refresh single-use rotation, challenge single-use/TTL, app-attest
  sign-count monotonicity, AIMD/refill math, ticket status transitions.
- Writer/pool/indexer decision logic moves out of I/O loops into pure
  functions (the `dim-tickets/src/plan.rs` pattern, which sits at 100%) and
  gets example-based units; the residual I/O shells are accepted as
  demo-covered.
- Coverage measurement is repeatable by anyone: `just coverage` (offline)
  and `just coverage-full` (merged with live suites), documented, matching
  what produced the baseline above.
- Every new live suite is wired into `just test-live` so it actually runs.

Success is binary: the phase demos — each suite green, and a re-run of
`coverage-full` showing the targeted files moved (expect the workspace to
land in the mid-70s; the number is a diagnostic, not the goal).

# Non-goals

- **No CI coverage gate or % threshold.** CI keeps the plain
  fmt/clippy/test gate; a threshold invites gaming and the live-DB CI
  dependency is still deliberately deferred. Revisit after this plan lands.
- **No mocking of the `submit_and_watch` transaction lifecycle.** Replaying
  subscription notifications through `MockRpcClient` is high-effort, low
  value per line; chain submission stays covered by the live-Paseo demos
  already recorded in the dim/invite plans.
- **No storage-read mocks** (`state_getStorage` SCALE fixtures) for
  `available_invites` etc. — fiddly, marginal. Exception: the indexer
  golden-block replay is *recorded as a future option* (Phase 4), not built.
- **Not chasing zeros that don't matter**: bin `main()`s, infinite loops
  (`run_forever`, `run_loop`, `sync::run`), `connect()` builder chains,
  metrics/health plumbing, generated `chain-types::people` internals.
- No property-based tests for their own sake — every test added here pins a
  concrete captured or designed invariant.

# Design principles

- **Test the decision, not the plumbing.** Where logic is inline in an I/O
  loop, hoist it into a pure function next to the existing pure core
  (`plan.rs`, `projection::decode_consumer`, `classify_submit_failure`) and
  unit-test that. Don't build mocks to reach logic that can simply move.
- **Reuse the three established patterns**, never invent a fourth:
  1. live-Postgres `#[ignore]` suites (`IDENTITY_TEST_DATABASE_URL` /
     `DATABASE_URL`, migrations + self-cleanup);
  2. offline production-router tests (`legacy_wire.rs` pattern: lazy dead
     pool + `ChainClient::from_online` over a `MockRpcClient` answering only
     `chain_getBlockHash`);
  3. plain units in `#[cfg(test)]` mods.
- **Unify before testing twice**: the byte-identical `ItemCompleted`/
  `ItemFailed` event filter (dim `chain.rs:134-143` == invite
  `chain.rs:96-105`) and the ss58-prefix ≥ 16384 check (indexer
  `bootstrap.rs:99` == `incremental.rs:212`) get extracted once, tested
  once.
- Env-parsing tests must not race: env vars are process-global, so either
  extract a pure `parse_x(raw: &str)` core (preferred where branchy) or run
  all env cases sequentially inside a single `#[test]`.

# Foundations / Already shipped (✅)

- Live-PG `#[ignore]` suites: identity-service ×7, invite-tickets
  `claim_live_pg`, username-indexer `pagination_live`/`poc_gate_live`;
  compose test DBs and the `just test-live` orchestration.
- Offline production-router replay: `legacy_wire.rs` in four crates, with
  `dead_chain_client()` MockRpcClient helpers (dim, indexer,
  identity-service). `from_online` seams exist in dim + indexer chain
  clients (missing in invite-tickets — 3-line add).
- Pure-core precedent: `dim-tickets/src/plan.rs` (100%),
  `projection::decode_consumer`, `writer::classify_submit_failure`,
  `pool::settle_batch_size` — all already unit-tested.
- `examples/auth_smoke.rs` has working challenge+sr25519 proof construction
  to reuse for token-route tests.

# MVP phases (ship-shaped, demoable)

## Phase 0: repeatable measurement
- **Goal**: anyone can reproduce the baseline table.
- **Scope checklist**:
  - [x] (2026-08-05) justfile `coverage` recipe: `cargo llvm-cov --workspace`
        (offline suite only, summary output).
  - [x] (2026-08-05) justfile `coverage-full` recipe: `_coverage-offline`
        (clean + `--no-report` workspace run), then every live suite via the
        shared `_live-suites` orchestration, then `cargo llvm-cov report
        --summary-only`. `test-live` now delegates to `_live-suites "cargo
        test"` so the suite list can't drift between the two.
  - [x] (2026-08-05) Prereqs documented on the recipes (cargo-llvm-cov
        prebuilt-release link + `rustup component add llvm-tools`).
  - [x] (2026-08-05) Found while wiring: `claim_live_pg` (invite-tickets)
        and `poc_gate_live` (indexer) were in NO recipe — both now run in
        `_live-suites` against the compose DBs (:5435 / :5433).
        `claim_live_pg` also had a real race: each test truncates the shared
        table, so cargo's parallel test threads wiped each other's fixtures
        (2 of 3 tests failed multi-threaded, all green with
        `--test-threads=1`) — pinned single-threaded in the recipe and in
        the suite's doc header.
  - [x] (2026-08-05) Final `coverage-full` command sequence run end-to-end
        (clean → offline workspace → all 15 live suites → merged report);
        results below.
- **✅ Demo**: `just coverage-full` on a clean checkout reproduces ~63%.
- **Risks**: recipe drift when new live suites land — mitigated by phases
  below adding theirs to both `test-live` and `coverage-full` in the same
  change.

## Phase 1: pure wins (no infra, biggest %-per-hour) — code-complete 2026-08-05
- **Goal**: the code that needs no mocks and no DB stops being uncovered.
- **Scope checklist**:
  - [x] (2026-08-05) `identity-service/http/error.rs`: table-driven
        `into_response()` test over all eight variants (status × code ×
        message) + `From<sqlx::Error>` → Internal. **Finding**: the module
        doc promises 500s are "surfaced as an opaque 500", but `Internal`
        is `#[error(transparent)]` so the wrapped error text (sqlx messages
        etc.) reaches the wire as `message` — pinned as-is in
        `internal_message_currently_echoes_the_wrapped_error_text`; making
        it opaque is a wire change needing its own decision.
  - [x] (2026-08-05) `identity-service/config.rs`: pure cores extracted
        (`decode_jwt_secret`, `normalize_attester_public_key`,
        `decode_signing_digest`, `decode_play_integrity_{decryption,
        verification}_key`, `validate_device_check_pem`,
        `decode_payment_{master,amount}`, `split_list`) with thin env
        wrappers, identical error strings. 10 new units + one sequential
        env test for the fail-fast ordering, paired-keys rule, and the
        auth-enabled required matrix. Env tests serialize on a new
        `crate::ENV_MUTEX` (lib.rs, cfg(test)).
  - [x] (2026-08-05) `chain-types`: 5 units — builder nonce reaches the
        wire as SCALE `Compact` via subxt's own `CheckNonce`, immortal-era
        + zero-tip defaults, `Default` lockstep with `new()`, `Noop`
        encoders emit exactly `N::VALUE` / nothing (byte-level, distinct
        from the existing VALUE-const assertions), and all four
        `PeopleConfig` cache-delegation methods offline.
  - [x] (2026-08-05) `WriterConfig::from_env` (identity-service +
        dim-tickets): one sequential env `#[test]` each (identity's under
        `ENV_MUTEX`), covering happy path, defaults, whitespace-as-unset,
        per-var missing/invalid errors, bounded-u64 messages, and
        dim's cross-var retry_base/retry_max bound. **Findings** (as of
        2026-08-05): identity's then-existing `CHAIN_WRITER_PROXY_FOR` error
        didn't name the env var (dim's did) — both vars have since been
        removed in favour of derived proxying (#32, #46) — and its plain
        `env_u64` knobs silently default on garbage
        while the queue/payment knobs in the same function are strict.
- **✅ Demo**: all suites green (`cargo test -p identity-service -p
  chain-types -p dim-tickets`), clippy -D warnings clean.
- **Risks**: config extraction touches startup code — mechanical, but keep
  wrappers byte-compatible (same error strings; `Debug`-redaction test
  already pins the struct).

## Phase 2: auth lane (token issuance, rotation, replay guards) — code-complete 2026-08-05
- **Goal**: the security-critical branches at 0–17% are pinned.
- **Scope checklist**:
  - [x] (2026-08-05) New `tests/auth_live.rs` (live-PG `#[ignore]`),
        everything driven through the production router (real pool + the
        `legacy_wire.rs` dead-chain client): challenge 201 → token 200 →
        replay 401 (single-use); SQL-aged expired challenge → 401 with
        `consumed_at` still NULL (proves TTL rejected it, not the proof);
        unknown challenge → 401.
  - [x] (2026-08-05) Refresh rotation via `POST /api/v1/auth/token/refresh`:
        rotate 200 with a fresh pair, replacement row carries
        `account_id`/`app_from_official_store`/`platform("ios")` forward,
        spent-token replay 401, unknown 401, SQL-expired 401.
  - [x] (2026-08-05) `app_attest::store` invariants (via a `#[doc(hidden)]`
        `app_attest_store` re-export; the auth modules stay private):
        upsert→find roundtrip, re-registration resets `sign_count` and
        replaces material, `commit_sign_count` advances but refuses
        equal/backward/unknown — the token.rs race guard.
  - [x] (2026-08-05) Offline (non-`#[ignore]`, runs in `just check`):
        the 400 WRONG_DATA lanes for missing/non-base64/wrong-length
        `Auth-ClientId`/`Auth-ClientProof`/`Auth-Challenge` headers over a
        lazy dead pool.
  - [x] (2026-08-05) Verdict lanes through the router in hard mode instead
        of a `&Config` refactor (the router reaches every pre-check line
        with no mocks — smaller diff, same coverage): JWT claims bind the
        proven key; bad proof 401; soft mode issues with truthful
        `app_from_official_store=false` vs hard 403 INTEGRITY_FAILED for
        no evidence / unknown `Auth-Attestation-Type`; plus all nine
        pre-crypto rejection reasons (iOS/Android package allow-lists,
        missing payload/key-id headers, unregistered App Attest key,
        unconfigured digests, play-integrity-not-configured).
        **CRL-unavailable 503 deliberately not covered**: reaching it needs
        a parseable attestation chain plus a mocked Google CRL endpoint;
        the status/code mapping itself is unit-pinned in `http/error.rs`.
  - [x] (2026-08-05) iOS sign-count-race arm covered at the store level
        (`commit_sign_count` equal/backward → false); the full assertion
        path stays with the attestation slice.
  - [x] (2026-08-05) `auth_live` wired into `_live-suites` (so both
        `test-live` and `coverage-full` run it).
- **✅ Demo**: `cargo test -p identity-service --test auth_live -- --ignored`
  → 6 passed against live Postgres; the offline 400 lane green in the
  normal suite.
- **Risks**: proof construction needs the dev sr25519 flow — exists in the
  example; if it drifts from the route's expectations that's a finding, not
  a test bug.

## Phase 3: outbox + writer state machines — code-complete 2026-08-05
- **Goal**: every guarded transition that can touch the chain is pinned;
  writer decision logic becomes pure.
- **Scope checklist**:
  - [x] (2026-08-05) `tests/outbox_live.rs` (identity-service, live-PG, 5
        tests, parallel-safe via unique base/lease names): Conflict mapping
        for both insert lanes; `claim_due` picks RESERVED +
        gate-passed/ungated RETRY_AFTER oldest-first, excludes future gates
        and SUBMITTING (which `submitting()` sees); **stale guards — wrong
        epoch, foreign holder, expired lease, and post-takeover old
        guard — all return false on the `mark_*` mutations** while the
        live guard advances; the full lease lifecycle
        (acquire/self-retake-bumps-epoch/renew exact-match/expired-never-
        resurrects/takeover); `depth_by_status` zero-fill contract.
  - [x] (2026-08-05) `chain-client`: shared pure `batch_item_outcomes`
        replacing the byte-identical Utility filter in dim + invite
        `chain.rs` (interleaving unit incl. same-named event on a foreign
        pallet); invite-tickets `ChainClient` gained `from_online`.
  - [x] (2026-08-05) `identity-service/chain/writer.rs` pre-check
        extraction **deliberately dropped**: on reading, the un-extracted
        decisions are two-line equality matches (owner==candidate,
        owned-by-other) — the subtle decision (`classify_submit_failure`)
        is already pure and matrix-tested. Extracting the trivial matches
        would produce tautological tests; the loops/`submit()` stay
        demo-covered as planned.
  - [x] (2026-08-05) `dim-tickets` hoists into `plan.rs`: `decode_due`
        (malformed-row skip + in-flight mapping), `parse_batch` (SS58
        classification; DB call stays in tick), `batch_retry_counts` (the
        re-join, now O(n) first-wins with the impossible-miss
        `debug_assert`ed; release behavior unchanged), `apply_split`
        (+`Retry` with per-item delay). 7 new plan.rs units; `tick()` is
        now plumbing. Lease SQL exposed as `#[doc(hidden)]`
        `try_acquire_lease`/`heartbeat_lease` seams.
  - [x] (2026-08-05) `dim-tickets/tests/tickets_live_pg.rs` (10 tests, no
        truncation — unique per-call keys, parallel-safe): all eight
        `tickets.rs` sqlx fns incl. the `mark_registered`
        no-`updated_at` quirk and the fetch_due lease/network/inviter
        filters, plus lease exclusivity/epoch-bump/heartbeat semantics.
        Wired into `_live-suites` with the dim test DB (:5434).
  - [x] (2026-08-05) `invite-tickets`: `plan_refill` + `item_accepted`
        hoisted from `tick_pool` (+saturation units), `parse_pool_dims`
        core + units, MaintainerConfig Debug-redaction test (was
        untested), and `tests/pool_live_pg.rs` proving the advisory
        tick-lock excludes a second connection until `TickGuard` drops
        (seam: `POOL_LOCK_KEY`/`acquire_tick_lock`/`TickGuard` pub +
        doc(hidden)). Wired into `_live-suites`.
- **✅ Demo**: all live suites green against real Postgres (outbox 5,
  tickets 10, pool 1, auth 6); workspace offline suite + clippy -D
  warnings clean.
- **Learned the hard way (recorded for the next person)**: the compose dev
  stack's `identity-chain-writer` / `username-indexer` / `invite-tickets-pool`
  services share the test databases — a live writer races the suites
  (claims test outbox rows, holds the projection lock, contends the pool
  tick lock). Stop the app services (`docker compose stop identity-chain-writer
  username-indexer invite-tickets-pool ...`) before running live suites;
  only the `*-postgres` containers should be up. The pool maintainer was
  also found ticking against Paseo with an unfunded signer — stopped.

## Phase 4: indexer ingest side — code-complete 2026-08-05
- **Goal**: the write path of the indexer (0–45%) gets the same treatment.
- **Scope checklist**:
  - [x] (2026-08-05) Shared ss58-prefix check: `ss58::validate_prefix`
        replaces both inline `>= 16_384` sites (bootstrap + incremental);
        boundary unit pins it to `encode`'s own range (16,383 ok / 16,384
        rejected on both paths).
  - [x] (2026-08-05) New `tests/ingest_live.rs` (one sequential test —
        `sync_state` is a singleton row; original state restored): with NO
        checkpoint, `checkpoint_freshness` is None and
        `index_finalized_range` returns the zero report **without touching
        the (dead) chain**; with a checkpoint, `ensure_seeded`
        short-circuits to None before the chain; the checkpoint row maps
        field-for-field onto `FreshnessSnapshot`; with a checkpoint the
        incremental pass proceeds to the head fetch (errors on the dead
        chain — the checkpoint read is the last DB-only step); a held
        projection advisory lock skips the pass with Ok(None)
        (`PROJECTION_LOCK_ID` made pub for real lock contention). Wired
        into `_live-suites`.
  - [x] (2026-08-05) **Finding**: the negative-counter saturation in
        `checkpoint_freshness` (`unwrap_or(0)`) is unreachable through the
        database — `sync_state` CHECK-constrains every counter to >= 0 —
        so it is defense-in-depth, not testable behavior (noted in the
        test).
  - [x] (2026-08-05) `bootstrap.rs` `fold_scan` hoist and the
        `incremental.rs` 3-way-dispatch hoist **deliberately dropped**:
        the fold is four near-identical decode-stage match arms welded to
        subxt storage-entry types — abstracting them costs more machinery
        than the logic it would test, and the per-stage skip counting has
        no branching beyond `continue`. The dispatch is a thin
        `Some/None × Ok/Err` mapping onto already-tested
        `projection::decode_consumer`. Both stay with the
        golden-block-replay option below.
  - [x] (2026-08-05) Golden-block-replay recorded as the future option: a
        SCALE-encoded `System::Events` fixture served via MockRpcClient
        would cover `index_block`/`bootstrap_locked` end-to-end; justified
        only if the ingest path keeps growing.
- **✅ Demo**: `ingest_live` green against live Postgres; ss58 units green;
  clippy -D warnings clean.
- **Risks**: `index_block`/`bootstrap_locked` chain-touching residue stays
  uncovered by design — `decode_consumer` carries the row logic; the
  live-sync slice's compose demo covers the wiring.

# Result (2026-08-05, all phases complete)

Merged re-measurement after the four phases: **75.6% line / 71.4% function**
(was 63.4% / 59.3%). Workspace suite: 344 tests, 0 failed; every live suite
green under instrumentation; fmt + clippy -D warnings clean.

| Crate | Before | After | | Crate | Before | After |
|---|---|---|---|---|---|---|
| chain-types | 48.1% | 100% | | identity-service | 64.2% | 78.1% |
| jwt-verify | 100% | 100% | | http-common | 71.6% | 71.6% |
| apidoc-gen | 96.5% | 96.5% | | dim-tickets | 40.2% | 70.0% |
| chain-client | 92.5% | 93.8% | | username-indexer | 63.0% | 69.1% |
| notifications | 74.4% | 74.4% | | invite-tickets | 41.7% | 59.5% |
| turn | 71.7% | 71.7% | | | | |

Headline files: `auth/refresh.rs` 16.8→100, `auth/challenge.rs` 36.7→100,
`app_attest/store.rs` 0→100, `auth/token.rs` 12.5→72.1, `chain/outbox.rs`
37.9→98.3, `chain/lease.rs`→100, `http/error.rs` 0→100, `config.rs`
18.9→90.5, dim `tickets.rs` 38→98.4, dim `writer.rs` 19.2→61.9, dim
`plan.rs` 100, invite `pool.rs` 5.3→42.5, `ss58.rs`→100. The remaining
cold spots are the accepted ones: bin mains, run loops, `submit()`
tx-lifecycles, `bootstrap_locked`/`index_block` chain scans (golden-block
replay is the recorded future option), and the per-crate `config.rs`
`from_env` shells in dim/invite (their parse cores are unit-tested).

# Deliberately left uncovered (recorded so nobody "fixes" them)

- All `src/bin/*` entrypoints, `db.rs` pool constructors, infinite loops,
  reconnecting-client builder chains, metrics/health/middleware plumbing
  (`http-common/metrics.rs`, `/health` probe wiring) — demo/ops-covered.
- `notifications` provider edge paths (70/60%) — live APNs/FCM territory,
  owned by the notify slice.
- Full tx-lifecycle mocking and storage-read SCALE fixtures — see
  Non-goals.


# Follow-up: deterministic enforcement (2026-08-06)

The 75.6% / 71.4% result above remains the historical all-live-suite measurement. The canonical reproducible baseline is now offline + isolated Postgres only: cargo-llvm-cov 0.8.7 reports **73.15% line / 69.14% function** across 14,813 lines. The lower percentage reflects exclusion of mutable People Chain coverage, not removed tests.

Pull requests, deployable `main` pushes, and `v*` releases now require the 344-test offline gate and all 12 database suites / 33 tests. Run `just coverage-db` for the canonical report; `just coverage-full` adds optional People Chain suites, and provider smokes remain separate. Implementation details and validation are recorded in [test-coverage-enforcement.md](test-coverage-enforcement.md).
