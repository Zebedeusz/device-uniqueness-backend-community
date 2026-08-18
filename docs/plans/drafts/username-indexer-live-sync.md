> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.
>
> Stage: **drafts** (not yet scheduled). An optimization on top of the shipped `username-indexer` continuous sync.

# Goals
- New finalized username registrations become searchable within ~one finalization (seconds), instead of up to `SYNC_INTERVAL_SECS`.
- No missed blocks across RPC reconnects — the change must stay gap-proof.
- Reuse the existing incremental engine and checkpoint; add a trigger, not a new sync path.

# Non-goals
- Changing search semantics, the response/cursor shape, or any mobile contract.
- Touching `device-attestation`, the chain-writer, or the registration path.
- Replacing polling entirely — it stays as an always-on safety net.
- Client-facing real-time (pushing updates to the app) or sub-second SLAs.

# Design principles
- User journey drives order.
- The finalized-block stream is a **latency trigger**, not a new code path — it drives the existing `incremental::index_finalized_range`.
- The `sync_state` checkpoint remains the correctness backbone; the stream only lowers latency.
- Polling stays as a concurrent fallback so the stream is never a single point of failure.
- The single-writer advisory lock is preserved — replicas stay safe.

# User journey
1. A user registers a lite username through `device-attestation`.
2. The registration finalizes on People Chain.
3. `username-indexer` is notified of the finalized block within seconds.
4. It indexes the new consumer immediately (checkpoint → head).
5. Another user's prefix search returns the new username within seconds of finalization.

# Foundations / Already shipped (✅)

## Incremental indexing engine
- What exists: `incremental::index_finalized_range` processes `checkpoint+1..=head`, per-block atomic writes + checkpoint, guarded by `pg_try_advisory_lock`, returns `Ok(None)` when another writer holds the lock (`crates/username-indexer/src/incremental.rs`).
- ✅ Demo: prod logs `finalized resync complete from_block=… to_block=… blocks_processed=…`.
- Gaps: only invoked on a 30s timer, so search freshness lags up to `SYNC_INTERVAL_SECS`.

## Polling loop + bounded backoff + freshness
- What exists: `sync::run` ticks on `SYNC_INTERVAL_SECS`, calls the engine, updates the in-memory `Freshness`, and backs off on failure (`crates/username-indexer/src/sync.rs`).
- ✅ Demo: `/readyz` reports `freshness` with `lastFinalizedNumber` / `lastSyncedAt`.

## Reconnecting RPC transport
- What exists: `ChainClient` is built on subxt's `ReconnectingRpcClient` (`crates/username-indexer/src/chain.rs`); `device-attestation` has a recovery test.
- Gaps: reconnection restores the **transport**, not subscription continuity — a finalized stream can miss notifications across a reconnect, so gap recovery must come from the checkpoint catch-up.

## Checkpoint + bootstrap-on-empty
- What exists: `bootstrap::ensure_seeded` full-scans only when `sync_state` is empty; otherwise the loop resumes from the checkpoint (`crates/username-indexer/src/bootstrap.rs`).

# MVP phases (ship-shaped, demoable)

## Phase 1: Finalized-block-triggered sync
- **Goal**: drive the existing engine from the finalized-block stream so new usernames appear within seconds.
- **Scope checklist**:
  - [ ] Open a finalized-block stream with `chain.online().stream_blocks()` (subxt `online_client.rs:249`, "stream of finalized blocks") inside the sync task.
  - [ ] On each finalized-block notification, call `index_finalized_range` unchanged — it try-locks and processes `checkpoint..=head`.
  - [ ] Keep the `SYNC_INTERVAL_SECS` timer running concurrently as a safety-net trigger; both feed the same advisory-locked engine (duplicate triggers are harmless — idempotent).
  - [ ] Update `Freshness`/logging to record stream-driven passes distinctly from timer passes.
  - [ ] Add a config toggle (e.g. `FINALIZED_SUBSCRIPTION`) that falls back to polling-only when off or when the stream can't be established.
- **✅ Demo**: register a username via `device-attestation`; it appears in `GET /api/v1/usernames/search` within a few seconds (well under the poll interval), verified on staging/prod.
- **Risks / failure modes**:
  - Silent stream stall → mitigated by the retained timer (still catches up).
  - Stream error/close → fall back to the timer + reconnecting client (Phase 2 hardens rebuild).
  - Duplicate/overlapping triggers → no-op (idempotent upserts, single advisory lock).

## Phase 2: Stream resilience + fallback
- **Goal**: make the stream self-healing and provably gap-proof.
- **Scope checklist**:
  - [ ] Handle stream end/error: log, then rebuild the stream with the existing bounded exponential backoff (`sync::backoff`).
  - [ ] Confirm reconnect gaps are closed — the next trigger's `checkpoint..=head` catch-up must cover any blocks finalized during the gap; no block skipped.
  - [ ] Structured logs for stream connected / dropped / reconnected and notifications processed.
  - [ ] Expose a freshness/lag signal (seconds since last finalized notification) for observability.
- **✅ Demo**: drop the RPC connection mid-run; confirm the stream rebuilds and blocks finalized during the gap are caught up (checkpoint advances, no missing accounts), search stays correct.
- **Risks / failure modes**:
  - Catch-up regression would reintroduce gaps → guard with a contract test / manual verify.

## Phase 3: Adaptive catch-up (replay vs snapshot) + rebuild
- **Goal**: bound worst-case catch-up and survive a pruned RPC by snapshotting current state instead of replaying a huge block range.
- **Scope checklist**:
  - [ ] Each pass, compute `lag = head - checkpoint`.
  - [ ] `lag <= CATCHUP_REPLAY_MAX_BLOCKS` → replay incrementally (today's path — cheap, precise).
  - [ ] `lag > CATCHUP_REPLAY_MAX_BLOCKS` → snapshot fast-forward: reuse `bootstrap`'s merge (upsert-by-`account_id` + reconcile + set the checkpoint to the snapshot's finalized block), then resume incremental. No drop, no empty window.
  - [ ] Fall back to the snapshot path when a replay fails with "state unavailable" (pruned node) errors on an old block.
  - [ ] Add a manual `rebuild` startup mode / command that forces the snapshot fast-forward against the current finalized head.
  - [ ] Log which path ran (replay vs snapshot) and why (lag / pruned-state error / manual).
- **✅ Demo**: stop the indexer, let the chain advance past the threshold, restart; confirm it snapshot-fast-forwards (one full scan, checkpoint jumps to head) instead of replaying, search stays correct, and incremental resumes.
- **Risks / failure modes**:
  - Threshold too low → needless full scans; too high → slow replays. Tune from observed catch-up cost.
  - The snapshot path re-reads all rows (bounded, but the "wasteful" full scan) — acceptable because it only runs when far behind.

# Contracts (guardrails)
- Only finalized state is indexed — never non-finalized.
- Never miss a finalized block: the checkpoint catch-up must cover any stream gap.
- The single-writer advisory lock is preserved; N replicas stay safe.
- Search path, response fields, and cursor shape are unchanged.
- Restart still resumes from the checkpoint — no full re-scan.
- Catch-up never drops the projection or serves an empty window — the snapshot path merges (upsert-by-`account_id`, prune vanished), never truncates.
- The subscription is optional: if it fails, polling still makes progress (no hard dependency).

# Key decisions (decide early)
- Subscription primitive: subxt `client.stream_blocks()` (finalized stream, `online_client.rs:249-262`); each item triggers `index_finalized_range` against the current head.
- Keep polling **always on** as a safety net (decided) vs subscription-only (rejected — reconnect gaps + silent stalls).
- Catch-up strategy: replay incrementally under `CATCHUP_REPLAY_MAX_BLOCKS`; snapshot fast-forward (merge) above it or on pruned-state errors. Threshold is a block count (simple proxy for cost); tune from measurements.
- RPC node type: replaying old blocks needs their state — an archive node avoids pruned-state failures; the snapshot path removes that dependency for large gaps.
- Default the subscription **on with fallback**, or off until proven on staging? (decide before rollout).
- After the stream drives latency, likely raise `SYNC_INTERVAL_SECS` (e.g. to 60s) so the timer is purely a backstop.

# Testing
- Manual smoke per phase: register → measure time-to-searchable.
- Reuse existing unit tests; add a small pure test for the stream-error → backoff/rebuild decision where feasible without a live chain.
- No live-chain tests in the `just check` gate.

# Polish rounds (after MVP)

## Polish round 1: Latency & lag observability
- Add "seconds since last finalized notification" and indexed-lag to `/readyz`; surface a degraded signal / alert when the stream stalls beyond a threshold.
- ✅ Check-in demo: `/readyz` shows lag; stalling the stream produces a visible degraded state without breaking search.

# Later / Deferred
- Pushing updates to clients in real time — out of scope (the app confirms registration on-chain, not via search).
- Sub-second freshness SLAs.
- Removing polling entirely — revisit only if the stream proves reliable enough to be the sole trigger.
