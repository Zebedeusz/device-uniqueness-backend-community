> Stage: active. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Make `GET /api/v1/usernames/search` return assigned People Chain usernames in the exact shipping iOS and Android shape.
- Run as an independently deployable Rust service with its own Postgres database and People Chain connection.
- Index finalized `Resources::Consumers` state without reading identity-service tables.

# Non-goals
- Username registration writes, JWT issuance, platform attestation, notifications, TURN, invites, gateway implementation, or cutover.
- Changing mobile paths or payloads.
- Moving prefix search onchain.
- Replacing the working direct-chain `/usernames/available` implementation in the first MVP.

# Design principles
- User journey drives order.
- The finalized chain is authoritative; Postgres is a disposable read projection.
- Public search stays public and rate-limited.
- The service must start, fail, deploy, and recover independently of identity-service.

# User journey
1. A user registers a lite username through the existing identity-service.
2. The registration finalizes on People Chain.
3. Username-indexer indexes the finalized consumer.
4. Another user searches by prefix and sees the assigned username.

# Foundations / Already shipped (✅)

## Mobile search contract
- What exists: `crates/identity-service/src/usernames/search.rs` preserves the public path, Android `status=ASSIGNED` query, and empty response shape.
- ✅ Demo: the focused wire-contract tests pass for iOS and Android query forms.
- Gaps: results are always empty.

## Chain and legacy references
- What exists: `chain-types` contains the pinned People Chain type surface. The legacy reference is `paritytech/identity-backend`, under `apps/identity-backend/src/supervision/individuality-indexer/` and `apps/identity-backend/src/routes/v1/username/`.
- ✅ Demo: the legacy service can populate and query its `individuality_usernames` projection.
- Gaps: the implementation is coupled to the TypeScript monolith and its database.

# MVP phases (ship-shaped, demoable)

## Phase 1: Searchable finalized snapshot — **done 2026-07-14**
- **Goal**: bootstrap an independent database from finalized People Chain state and serve real search results.
- **Scope checklist**:
  - [x] Add an independently deployable username-indexer crate/binary with fail-fast configuration, liveness, and readiness.
  - [x] Add a service-owned migration for assigned username records and a deterministic prefix-search index.
  - [x] Scan finalized `Resources::Consumers` storage in bounded pages.
  - [x] Decode account ID, lite username, optional full username, and finalized onchain position; skip malformed records and log the failure.
  - [x] Upsert idempotently into the local projection.
  - [x] Serve public `GET /api/v1/usernames/search` with `prefix`, optional `status`, `limit`, `cursor`, and `includeOnchainData` compatibility.
  - [x] Preserve `{usernames, nextCursor}` and result fields used by the shipping clients.
- **✅ Demo**: run one bootstrap against Paseo Next v2, search a known prefix through the new service, and receive the assigned account and username in a response both mobile models decode. — **Passed 2026-07-14**: compose stack bootstrapped 29,762 finalized usernames (0 decode failures) from `wss://paseo-people-next-system-rpc.polkadot.io`; `GET /usernames/search?prefix=talles` returned assigned rows + a valid `nextCursor` in the mobile shape.
- **Risks / failure modes**:
  - Runtime metadata drift can break storage decoding.
  - A full scan must stay bounded in memory and database transaction size.
  - Prefix ordering and cursors must remain deterministic under concurrent upserts.

## Phase 2: Continuous finalized sync — **done 2026-07-14**
- **Goal**: make newly registered usernames discoverable without manual bootstrap.
- **Scope checklist**:
  - [x] Persist the last successfully indexed finalized block or an equivalent resumable checkpoint.
  - [x] Poll finalized state on a configurable interval with reconnect and bounded retry.
  - [x] Advance the checkpoint only after all projection writes for the observed state succeed.
  - [x] Expose readiness/freshness and structured logs for last finalized block, last successful sync, records indexed, and decode failures.
  - [x] Re-running the same finalized range must be idempotent.
- **✅ Demo**: register a fresh username with identity-service; after finalization and one configured sync interval, search returns it without restarting either service. — **Passed 2026-07-14**: registered `zdxphasetwo.71` (owner `5D7YrV5…mfzq`, chain-writer tx `0x8aaafb…091f`, outbox `ASSIGNED`); the untouched indexer surfaced it in `search?prefix=zdxphasetwo` ~15s after finalization.
- **Risks / failure modes**:
  - RPC interruption must not advance the checkpoint or erase the last good projection.
  - Reorg-sensitive state must never be indexed from a non-finalized block.

## Phase 3: Mobile compatibility completion — **done 2026-07-14**
- **Goal**: prove the read service can replace the legacy search route without client changes.
- **Scope checklist**:
  - [x] Golden fixtures for iOS prefix-only and Android `status=ASSIGNED` requests.
  - [x] Pagination tests covering stable ordering, cursor continuation, and no duplicate rows.
  - [x] Rate limit public search without adding JWT or proof-of-compute requirements.
  - [x] Add the username-indexer container and service-owned environment variables without changing identity-service deployment state.
- **✅ Demo**: contract tests pass, a mobile-compatible request returns a newly indexed username, and stopping username-indexer does not affect auth or registration readiness. — **Passed 2026-07-14**: offline golden/cursor tests + `just test-live` pagination all green; the indexer stayed healthy and served search while the identity DB volume was wiped and the identity services rebuilt, confirming full decoupling from auth/registration.
- **Risks / failure modes**:
  - Gateway routing is deliberately deferred; test the service directly in this phase.

# Contracts (guardrails)
- Search remains `GET /api/v1/usernames/search` and remains public.
- Android's `status=ASSIGNED` query must be accepted.
- The response keeps `usernames`, `nextCursor`, `accountId`, `username`, `status`, `createdAt`, `updatedAt`, and optional `onchainData` compatibility. **Verified against the shipping clients:** iOS (`polkadot-app-ios-v2`) omits `onchainData` from its decoder entirely and requires non-null `createdAt`/`updatedAt`; Android (`polkadot-app-android-v2`) decodes `onchainData` as nullable and never consumes it. Neither client sends `includeOnchainData`, so the always-`null` projection value is compatible.
- Read only finalized People Chain state.
- Never read or write identity-service tables.
- A stale index may affect search freshness, but it must not affect registration.

# Key decisions (decide early)
- Checkpoint strategy: **decided — incremental finalized-block, event-signalled indexing.** One full `bootstrap` runs once at startup to seed the projection, reconcile it (`DELETE ... WHERE snapshot_hash <> $1`), and set the `sync_state` checkpoint. The resync loop then polls the finalized head every `SYNC_INTERVAL_SECS` and, for each new finalized block from `checkpoint+1..=head`, decodes `Resources` events (`LitePersonRegistered` / `PersonRegistered` / `IdentifierKeyUpdated`, plus `PeopleLite::ConsumerRegistered` as a duplicate-safe signal) to learn which accounts changed — the events carry only an account, never the username — re-reads `Resources::Consumers(account)` at that block for the authoritative username, and upserts it (or deletes the local row if the entry is now absent) inside a single transaction that also advances the per-block checkpoint. Cost therefore scales with the number of changed accounts, not the total size of `Consumers`. Only block numbers up to the finalized head are processed, so no reorg-sensitive state is indexed; reprocessing a block re-reads authoritative storage and upserts by `account_id` primary key, so repeats are idempotent no-ops. In this mode `sync_state.last_finalized_*` is the per-block checkpoint and each `assigned_usernames.snapshot_hash` records the last block that touched that row (diverging from bootstrap's single shared snapshot value); that is expected and harmless because incremental never runs the bootstrap reconciliation delete.
- Writer serialization: **decided — single active writer via a Postgres advisory lock.** Bootstrap and the incremental loop share one lock id; bootstrap takes it with a blocking `pg_advisory_lock` and **runs a full snapshot only when the `sync_state` checkpoint is empty**, and each incremental pass takes it with `pg_try_advisory_lock` (skipping the pass if another instance holds it). This makes the service safe to run as N replicas against one shared Postgres, and makes restarts resume incrementally from the checkpoint instead of re-scanning the whole map.
- Cursor encoding and stable sort key. Implemented as `(lower(display_username) COLLATE "C", lite_base, lite_digits::numeric, account_id)` — digits sort **numerically** (`foo.2` before `foo.10`), guarded by `just test-live`. **Open:** confirm the legacy TS `/usernames/search` orders digits the same way (the "lowercased username ordering" guardrail), or record the intentional difference.
- `/usernames/available` placement: **decided — stays in `identity-service`; does NOT move into this service.** Availability is an exact, authoritative, JWT-gated direct-chain read (`Resources::UsernameOwnerOf` for `01..99`); this indexer is a disposable, eventually-consistent, public projection that lags a sync interval — answering availability from it would hand out stale "available" results that fail at registration, and would force a JWT/auth surface onto a deliberately public service. `docs/architecture.md` keeps `availability` inside `identity-service` (single identity failure/secret boundary). This indexer owns prefix search/autocomplete only; exact availability's future direction is on-device (deferred, see below), not here.

# Testing
- Manual smoke demo per phase.
- Storage decoding fixtures from the pinned People Chain metadata.
- Database tests for idempotent upsert, deterministic prefix order, and pagination.
- Mobile wire fixtures for empty and non-empty search results.
- Live-Postgres tests (`crates/username-indexer/tests/pagination_live.rs`) are `#[ignore]`d and excluded from `just check` (a live-Postgres CI dependency is deliberately deferred). Run them on demand with `just test-live`, which spins up the indexer Postgres, runs the ignored tests, and tears it down. This lane guards keyset pagination ordering — it caught and now regression-guards the numeric digit-order fix (commit `50be667`: `foo.2` before `foo.10`).

# Polish rounds (after MVP)

## Polish round 1: Search performance and recovery
- Benchmark bootstrap time, sync time, and p50/p95 search latency against the legacy route.
- Add a safe rebuild command or startup mode for recreating the disposable projection.
- ✅ Check-in demo: rebuild from an empty database, catch up to the finalized head, and meet the agreed search latency/freshness target.

# Later / Deferred
- Gateway path routing and production cutover live in later plans.
- On-device exact availability and any People Chain storage redesign require client/runtime decisions outside this slice.