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
3. Username-read indexes the finalized consumer.
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

## Phase 1: Searchable finalized snapshot
- **Goal**: bootstrap an independent database from finalized People Chain state and serve real search results.
- **Scope checklist**:
  - [ ] Add an independently deployable username-read crate/binary with fail-fast configuration, liveness, and readiness.
  - [ ] Add a service-owned migration for assigned username records and a deterministic prefix-search index.
  - [ ] Scan finalized `Resources::Consumers` storage in bounded pages.
  - [ ] Decode account ID, lite username, optional full username, and finalized onchain position; skip malformed records and log the failure.
  - [ ] Upsert idempotently into the local projection.
  - [ ] Serve public `GET /api/v1/usernames/search` with `prefix`, optional `status`, `limit`, `cursor`, and `includeOnchainData` compatibility.
  - [ ] Preserve `{usernames, nextCursor}` and result fields used by the shipping clients.
- **✅ Demo**: run one bootstrap against Paseo Next v2, search a known prefix through the new service, and receive the assigned account and username in a response both mobile models decode.
- **Risks / failure modes**:
  - Runtime metadata drift can break storage decoding.
  - A full scan must stay bounded in memory and database transaction size.
  - Prefix ordering and cursors must remain deterministic under concurrent upserts.

## Phase 2: Continuous finalized sync
- **Goal**: make newly registered usernames discoverable without manual bootstrap.
- **Scope checklist**:
  - [ ] Persist the last successfully indexed finalized block or an equivalent resumable checkpoint.
  - [ ] Poll finalized state on a configurable interval with reconnect and bounded retry.
  - [ ] Advance the checkpoint only after all projection writes for the observed state succeed.
  - [ ] Expose readiness/freshness and structured logs for last finalized block, last successful sync, records indexed, and decode failures.
  - [ ] Re-running the same finalized range must be idempotent.
- **✅ Demo**: register a fresh username with identity-service; after finalization and one configured sync interval, search returns it without restarting either service.
- **Risks / failure modes**:
  - RPC interruption must not advance the checkpoint or erase the last good projection.
  - Reorg-sensitive state must never be indexed from a non-finalized block.

## Phase 3: Mobile compatibility completion
- **Goal**: prove the read service can replace the legacy search route without client changes.
- **Scope checklist**:
  - [ ] Golden fixtures for iOS prefix-only and Android `status=ASSIGNED` requests.
  - [ ] Pagination tests covering stable ordering, cursor continuation, and no duplicate rows.
  - [ ] Rate limit public search without adding JWT or proof-of-compute requirements.
  - [ ] Add the username-read container and service-owned environment variables without changing identity-service deployment state.
- **✅ Demo**: contract tests pass, a mobile-compatible request returns a newly indexed username, and stopping username-read does not affect auth or registration readiness.
- **Risks / failure modes**:
  - Gateway routing is deliberately deferred; test the service directly in this phase.

# Contracts (guardrails)
- Search remains `GET /api/v1/usernames/search` and remains public.
- Android's `status=ASSIGNED` query must be accepted.
- The response keeps `usernames`, `nextCursor`, `accountId`, `username`, `status`, `createdAt`, `updatedAt`, and optional `onchainData` compatibility.
- Read only finalized People Chain state.
- Never read or write identity-service tables.
- A stale index may affect search freshness, but it must not affect registration.

# Key decisions (decide early)
- Checkpoint strategy: periodic full finalized snapshots first, or incremental finalized-block/event indexing from the start.
- Cursor encoding and stable sort key, compatible with the legacy lowercased username ordering.
- Whether `/usernames/available` moves into this service after search MVP; the current endpoint already works and is not a Phase 1 dependency.

# Testing
- Manual smoke demo per phase.
- Storage decoding fixtures from the pinned People Chain metadata.
- Database tests for idempotent upsert, deterministic prefix order, and pagination.
- Mobile wire fixtures for empty and non-empty search results.

# Polish rounds (after MVP)

## Polish round 1: Search performance and recovery
- Benchmark bootstrap time, sync time, and p50/p95 search latency against the legacy route.
- Add a safe rebuild command or startup mode for recreating the disposable projection.
- ✅ Check-in demo: rebuild from an empty database, catch up to the finalized head, and meet the agreed search latency/freshness target.

# Later / Deferred
- Gateway path routing and production cutover live in later plans.
- On-device exact availability and any People Chain storage redesign require client/runtime decisions outside this slice.