> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- A contributor can run one deterministic command that provisions isolated scratch Postgres instances, executes all 33 database-backed ignored tests, and removes only its own test resources (`justfile:18-76`).
- Pull requests and deployable `main` pushes cannot pass while any offline or database-backed regression test fails (`.github/workflows/deploy.yml:8-22`).
- A `v*` release cannot build or publish an image until the same required checks have passed for the tagged commit (`.github/workflows/release.yml:6-46`).
- Challenge consumption and refresh rotation prove the real security contract under simultaneous replay: exactly one request succeeds and exactly one replacement session is created (`crates/identity-service/src/auth/challenge.rs:63-72`, `crates/identity-service/src/auth/refresh.rs:93-145`).
- Coverage reporting clearly separates deterministic offline+Postgres coverage from optional live People Chain and push-provider smokes (`justfile:78-94`, `docs/plans/done/test-coverage.md:331-364`).

# Non-goals
- No line/function coverage threshold in CI; this plan enforces behavioral suites, not a percentage target (`docs/plans/done/test-coverage.md:59-63`).
- No People Chain transaction-lifecycle mock, storage-read SCALE fixture, or indexer golden-block replay (`docs/plans/done/test-coverage.md:64-70`, `docs/plans/done/test-coverage.md:321-329`).
- No APNs or FCM credentials in CI; their existing provider smokes remain explicit operator tests (`crates/notifications/tests/apns_live.rs`, `crates/notifications/tests/fcm_live.rs`).
- No public API, schema, migration, service-boundary, or production-runtime behavior change.
- No rewrite of the existing unit, golden-wire, or live-Postgres test styles documented in `docs/plans/done/test-coverage.md:77-93`.

# Design principles
- User journey drives order.
- Make the daily required path hermetic before adding it to CI.
- Reuse the current live-suite commands and Compose healthchecks; extract only enough orchestration for local and CI callers to share one suite list (`justfile:24-76`, `docker-compose.yml:20-33`).
- Fail closed: missing prerequisites, unhealthy databases, and skipped assertions must make the required gate red.
- External chain state and provider credentials are useful smoke coverage, but never prerequisites for a deterministic merge/release gate.
- Test concurrency properties with concurrent requests, not sequential replays.

# User journey
1. A contributor runs `just check` for the fast offline gate, then `just test-live-db` for all deterministic SQL/state-machine behavior.
2. The test command starts a project-scoped scratch stack on test-only loopback ports, waits for all four databases, runs the shared suite catalog, and tears that stack down without touching the contributor's normal `dub-paseo-next-v2` project.
3. A pull request runs the same offline and database-backed commands automatically.
4. A release tag reuses those required checks and publishes images only after they pass for the tagged SHA.
5. A maintainer opts into People Chain or APNs/FCM smokes through separately named commands when the required external dependencies are available.
6. A maintainer measures deterministic coverage with the offline+Postgres profile and can identify any optional external contribution separately.

# Foundations / Already shipped (✅)

## Fast offline gate
- What exists: `just check` maps to rustfmt, clippy with warnings denied, and `cargo test --workspace`; the same Rust commands run on pushes and pull requests (`justfile:3-16`, `.github/workflows/deploy.yml:17-22`).
- ✅ Demo: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` pass with 344 non-ignored tests.
- Gaps: SQL-heavy integration tests are compiled but ignored by this gate.

## Live suite inventory and measurement
- What exists: `_live-suites` already names the identity, indexer, invite, and DIM suites, and both `test-live` and `coverage-full` delegate to it (`justfile:18-94`).
- ✅ Demo: the current five remediation suites pass against Postgres: auth 6, outbox 5, DIM tickets 10, indexer ingest 1, invite pool lock 1.
- Gaps: the orchestration shares the normal Compose project/ports, removes pre-existing database containers, includes 10 external-RPC tests in the same command, and calls itself every ignored live suite although the 2 notification-provider smokes are separate.

## Security-critical SQL primitives
- What exists: challenge consumption is one conditional `UPDATE`; refresh rotation holds the presented row `FOR UPDATE` and creates the replacement in the same transaction (`crates/identity-service/src/auth/challenge.rs:63-72`, `crates/identity-service/src/auth/refresh.rs:99-144`).
- ✅ Demo: existing router tests prove sequential success/replay and expiry behavior (`crates/identity-service/tests/auth_live.rs:126-211`, `crates/identity-service/tests/auth_live.rs:347-426`).
- Gaps: no test sends the same challenge or refresh token simultaneously.

# MVP phases (ship-shaped, demoable)

## Phase 1: trustworthy local database gate
- **Goal**: make the database-backed suite safe and deterministic enough to run every day and later reuse unchanged in CI.
- **Scope checklist**:
  - [x] Split the current catalog into a required DB-only set (12 suites / 33 tests) and explicitly optional sets: People Chain (3 suites / 10 tests) and notification providers (2 tests). Keep suite names in one shared orchestration entrypoint so local, coverage, and CI callers cannot drift (`justfile:40-76`).
  - [x] Add `just test-live-db` as the canonical required command; retain separately named opt-in commands for the current People Chain and provider smokes. Make `just test-live` either a documented alias of the DB gate or remove its ambiguity in `README.md:26-34` and `AGENTS.md` Commands.
  - [x] Run the four existing Postgres services under a dedicated Compose project name, using the existing debug overlay with four test-only host ports. Use project-scoped named volumes from `docker-compose.yml:529-536`; never stop, remove, or connect to the normal dev project's containers.
  - [x] Replace readiness loops that fall through after 30 attempts with a checked health wait; database startup failure must abort before Cargo runs (`justfile:35-38`, `justfile:50-53`, `justfile:61-64`, `justfile:71-74`).
  - [x] Tear down the dedicated test project with its volumes on success, failure, or interruption; do not issue `rm` against services in the default project (`justfile:33-34`).
  - [x] Change `ingest_database_branches_work_without_a_chain` to require `DATABASE_URL` instead of returning success when it is absent (`crates/username-indexer/tests/ingest_live.rs:66-81`).
  - [x] Preserve the single-thread constraint for `claim_live_pg`, whose per-test `TRUNCATE` is not parallel-safe (`crates/invite-tickets/tests/claim_live_pg.rs:66`, `justfile:65-68`).
- **✅ Demo (2026-08-06)**: with the normal dev Compose project already running, `scripts/run_live_suites.sh db cargo test` passed all 33 tests; before/after container IDs for the dev project were identical; the dedicated test project and volumes were absent afterward; directly running the ingest test without `DATABASE_URL` failed rather than passing.
- **Risks / failure modes**:
  - Fixed test ports can collide with another test run; fail with a clear port/project error rather than falling back to the dev databases.
  - Cleanup must be scoped by an explicit test project name so an empty or inherited variable cannot target the default project.

## Phase 2: concurrent replay regression coverage
- **Goal**: pin the one-winner security property that the current sequential auth tests do not exercise.
- **Scope checklist**:
  - [x] Extend `auth_live.rs` with two identical token requests issued concurrently against one fresh challenge; assert sorted statuses `[200, 401]`, one consumed challenge, and one refresh row for the proven account (`crates/identity-service/tests/auth_live.rs:126-211`).
  - [x] Extend the refresh test with two concurrent rotations of the same presented token; assert sorted statuses `[200, 401]`, one `replaced_by` value, and exactly one replacement row carrying the original claims (`crates/identity-service/tests/auth_live.rs:347-426`).
  - [x] Keep these as production-router/live-Postgres tests so they exercise the actual SQL transaction and HTTP mapping rather than a helper mock (`crates/identity-service/tests/auth_live.rs:54-60`).
  - [x] Use a barrier or equivalent synchronization so both requests are ready before release; a merely spawned but effectively sequential test does not satisfy this phase.
- **✅ Demo (2026-08-06)**: the synchronized concurrency cases passed in both the normal database gate and the instrumented coverage run; each produced exactly one winner, with persisted row-count and replacement assertions confirming no duplicate session.
- **Risks / failure modes**:
  - Random scheduling can make a weak concurrency test pass without overlap; synchronize request release and assert persisted row counts.
  - Cleanup must remain per-account/per-token so parallel Cargo test threads cannot delete another test's fixtures (`crates/identity-service/tests/auth_live.rs:119-124`).

## Phase 3: enforce the same gate on pull requests, deploys, and releases
- **Goal**: no merge/deploy/release path can bypass the deterministic test contract.
- **Scope checklist**:
  - [x] Add a database-test CI job that invokes the exact Phase 1 orchestration rather than duplicating its suite list. Run it on every pull request and `main` push alongside the existing offline `check` job (`.github/workflows/deploy.yml:3-22`).
  - [x] Make the image build/deploy path depend on both the offline and database checks; currently `build` depends only on path classification and can push an image before `check` succeeds (`.github/workflows/deploy.yml:52-59`).
  - [x] Reuse the required check workflow from the `v*` release workflow, or otherwise make release `build` depend on an equivalent offline+database check for the tagged SHA (`.github/workflows/release.yml:6-46`). Prefer one reusable workflow over two copied command lists.
  - [x] Keep People Chain and provider smokes out of required PR/release jobs; expose them as manual/scheduled follow-up workflows only if an existing credentialed environment is available.
  - [x] Remove the duplicated release manifest inspection step while touching the dependency graph (`.github/workflows/release.yml:96-99`).
- **✅ Demo (2026-08-06, static/local)**: the reusable workflow parses successfully and contains the offline-to-database dependency; deploy image build requires both `check` and path classification, while release image build requires `check`. The shared database command passed locally. Remote Actions execution and image publication remain deploy-time verification.
- **Risks / failure modes**:
  - A reusable-workflow refactor can accidentally weaken existing path filters or deploy dependencies; retain the current docs-only no-deploy behavior (`.github/workflows/deploy.yml:24-54`).
  - Docker startup time can lengthen CI; keep the DB job parallel with offline compilation and cache Cargo artifacts, but do not skip suites to recover time.

## Phase 4: honest, repeatable coverage reporting
- **Goal**: make the quoted coverage number correspond to a deterministic suite contributors and CI can reproduce.
- **Scope checklist**:
  - [x] Add a canonical offline+DB coverage recipe that runs `_coverage-offline`, then the Phase 1 DB catalog under `cargo llvm-cov --no-report`, then emits `--summary-only` (`justfile:78-94`).
  - [x] Rename or document the current `coverage-full` recipe so its optional People Chain contribution cannot be mistaken for the deterministic baseline; do not imply APNs/FCM smokes are included.
  - [x] Re-measure the deterministic total and per-crate line/function coverage with cargo-llvm-cov 0.8.7, recording the command and result in this plan when it moves to `done/`.
  - [x] Update `README.md` and `AGENTS.md` with the required DB gate, optional external smokes, and canonical coverage command. Add a historical note to `docs/plans/done/test-coverage.md` rather than rewriting its 2026-08-05 measurement.
- **✅ Demo (2026-08-06)**: from a clean profile, the canonical offline and database recipe passed with no People Chain RPC or push-provider credential and reported 14,813 instrumented lines at 73.15% line / 69.14% function coverage.
- **Risks / failure modes**:
  - Toolchain or compiler changes can move instrumentation totals; pin the measurement tool/version in the recorded result, but do not convert the number into a CI threshold.
  - Coverage must merge profiles from the DB runs rather than producing one report per suite.

# Contracts (guardrails)
- `docker-compose.yml` remains host-port-free; test ports live in an overlay and bind only to `127.0.0.1` (`AGENTS.md` Gotchas, `scripts/verify_compose_boundaries.sh`).
- The required database gate never contacts People Chain, APNs, or FCM and never needs production credentials.
- The test command never stops or removes resources in the normal dev Compose project.
- Missing test database URLs and failed health checks are hard failures, never successful skips.
- Challenge and refresh replays admit exactly one winner even when requests overlap.
- `claim_live_pg` remains single-threaded until its global `TRUNCATE` setup is replaced.
- Migrations continue to run through each crate's existing `sqlx::migrate!` startup/test path; no manual migration command or `.sqlx` cache is added (`AGENTS.md` Conventions/Gotchas).
- Release image build and manifest publication require both offline and DB checks for the tagged commit.
- Optional external smokes are clearly labeled and do not contribute to the canonical deterministic coverage claim.

# Key decisions (decide early)
- **Canonical gate name**: make `test-live-db` explicit and decide whether legacy `test-live` aliases it or becomes an all-external umbrella. Recommendation: alias it to the deterministic DB gate and use explicit `test-live-chain` / `test-live-providers` names.
- **Shared orchestration location**: CI cannot currently invoke `just`; either install `just` in the reusable workflow or extract the current shell body to one `scripts/` entrypoint called by both CI and `justfile`. Recommendation: one script, avoiding tool installation and duplicated suite catalogs.
- **Compose isolation**: reuse `docker-compose.yml` plus `docker-compose.debug.yml` under an explicit test project and alternate ports; do not create a parallel schema/topology unless the overlay cannot safely express isolation.
- **Workflow reuse**: recommendation is one reusable required-check workflow called by deploy and release, so tagged releases cannot drift from PR checks.
- **Coverage baseline**: the deterministic offline+DB measurement becomes canonical; People Chain/provider runs remain supplemental and are reported separately.

# Testing
- Manual smoke demos per phase.
- Minimal regression tests only for contracts.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` after Rust changes.
- `cargo test --workspace` for the 344-test offline gate.
- `just test-live-db` for the 33 deterministic ignored tests, including the new concurrency cases once added.
- `scripts/verify_compose_boundaries.sh` after Compose/orchestration changes.
- A CI dependency-graph check proving build/deploy/release jobs cannot start before required checks pass.
- `just test-live-chain` and provider smokes only when explicitly validating their external environments.

# Result (2026-08-06, all MVP phases complete)

The deterministic merge/release gate now consists of the 344-test offline workspace suite plus 12 Postgres-backed suites / 33 ignored tests. Local and CI callers share `scripts/run_live_suites.sh`; its per-run Compose project, alternate loopback ports, checked health wait, and scoped cleanup leave the normal dev stack untouched. People Chain and provider smokes remain explicit optional lanes.

Challenge consumption and refresh rotation now release two identical production-router requests through a barrier and prove one HTTP winner plus one persisted session/replacement. Deploy and tag-release image builds reuse `.github/workflows/check.yml` and cannot start before the offline and database jobs pass.

Canonical coverage was measured with cargo-llvm-cov 0.8.7:

```sh
cargo llvm-cov clean --workspace
cargo llvm-cov --no-report --workspace
scripts/run_live_suites.sh db cargo llvm-cov --no-report
cargo llvm-cov report --summary-only
```

Total: **73.15% line / 69.14% function** across 14,813 instrumented lines. This is intentionally lower than the historical 2026-08-05 "full" figure because the canonical baseline excludes mutable People Chain suites.

| Crate | Line | Function |
|---|---:|---:|
| apidoc-gen | 96.47% | 98.53% |
| chain-client | 90.12% | 75.00% |
| chain-types | 100.00% | 100.00% |
| dim-tickets | 69.99% | 69.09% |
| http-common | 71.61% | 65.28% |
| identity-service | 73.07% | 66.84% |
| invite-tickets | 59.55% | 57.25% |
| jwt-verify | 100.00% | 100.00% |
| notifications | 74.41% | 68.71% |
| turn | 71.74% | 66.67% |
| username-indexer | 69.09% | 70.22% |

Validation passed: rustfmt check, Clippy with warnings denied, all 344 offline tests, all 33 deterministic database tests both normally and under coverage, Compose boundary verification, workflow YAML parsing, missing-`DATABASE_URL` ingest failure, unsafe project-name rejection, and missing-provider-credential rejection. External People Chain, APNs, and FCM smokes were not run because they are deliberately non-deterministic/operator-credentialed.

# Polish rounds (after MVP)

## Polish round 1: faster CI without weaker coverage
- Measure DB provisioning, compilation, and suite durations; reuse Cargo caching and run independent service databases in parallel only where fixture isolation already permits it.
- Keep `claim_live_pg` serialized and keep one shared suite catalog.
- ✅ Check-in demo: the required CI gate is measurably faster across three runs with the same 33+ database tests and no new flakes.

## Polish round 2: scheduled external confidence
- If stable non-production credentials/endpoints are available, add non-required scheduled/manual jobs for People Chain, APNs, and FCM smokes with explicit secret guards and actionable skip/failure reporting.
- Keep their results separate from the deterministic merge/release gate and baseline coverage.
- ✅ Check-in demo: a manual workflow reports each external lane independently, and absence of credentials is shown as an explicit skipped job rather than a passing test.

# Later / Deferred
- A CI coverage percentage threshold; revisit only after the deterministic baseline has remained stable and the team chooses a regression policy.
- Golden-block replay for `bootstrap_locked`/`index_block`; revisit if indexer ingestion logic grows or a production ingest regression occurs (`docs/plans/done/test-coverage.md:321-329`).
- Mocked `submit_and_watch` and storage reads; revisit if live-chain failures become frequent or transaction classification changes (`docs/plans/done/test-coverage.md:64-70`).
- Replacing `claim_live_pg`'s `TRUNCATE` with per-test fixtures; revisit if suite-level serialization becomes a material bottleneck.
- Release-device attestation and production APNs/FCM validation remain owned by their service/cutover plans, not this test-infrastructure slice (`docs/architecture.md` Remaining scope).
