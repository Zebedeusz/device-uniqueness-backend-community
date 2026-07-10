> Stage: done. Keep this plan as the completion record for the Identity Service MVP. New services and release work have separate plans indexed by `docs/roadmap.md`.

# Goals
- Let a mobile-compatible client obtain a JWT, check lite-username discriminators, and register a username on Paseo Next v2.
- Prove the async write path reaches finalized People Chain state through a durable outbox and a single chain writer.
- Preserve the shipping iOS and Android `/api/v1` wire contracts while platform attestation is disabled.

# Non-goals
- Real App Attest, DeviceCheck, Play Integrity, or Android TEE verification.
- Username prefix indexing, push delivery, TURN credentials, invite issuance, gateway routing, or legacy cutover.
- Production throughput optimization or multi-registration batching.

# Design principles
- User journey drives order.
- `who` equals the authenticated JWT subject.
- The outbox is the durable work record; unknown chain outcomes are reconciled before retry.
- One writer account has one active nonce lane.

# User journey
1. The client requests a challenge and proves control of its sr25519 account.
2. The service issues and rotates a JWT.
3. The client checks available digits for a base username.
4. The client submits a signed registration payload.
5. The writer finalizes `PeopleLite.attest`, and the account receives its username, consumer record, and Statement Store allowance.

# Foundations / Already shipped (✅)

## Rust service foundation
- What exists: the Cargo workspace, `identity-service`, `chain-types`, health routes, fail-fast configuration, Postgres migrations, and `just check`.
- ✅ Demo: `just check` passes; `/livez` and `/readyz` distinguish process, database, and People Chain health.
- Gaps: none for the Identity Service MVP.

## People Chain type surface
- What exists: static subxt codegen from `crates/chain-types/metadata/people.scale`, pinned to Paseo Next v2 runtime spec version `1000029`.
- ✅ Demo: generated storage and transaction payload tests pass.
- Gaps: metadata must be refreshed after an incompatible runtime upgrade.

# MVP phases (ship-shaped, demoable)

## Phase 1: Auth handshake ✅
- **Goal**: issue a mobile-compatible JWT with platform attestation disabled.
- **Scope checklist**:
  - [x] `POST /api/v1/auth/challenges` with a DB-backed, single-use challenge.
  - [x] iOS App Attest registration placeholder returning `202 {}`.
  - [x] `POST /api/v1/auth/token` with the shipping sr25519 proof contract.
  - [x] Single-use refresh-token rotation at `/api/v1/auth/token/refresh`.
  - [x] Ed25519 JWT signing and JWKS publication.
- **✅ Demo**: the auth smoke drives challenge → token → refresh and verifies the JWT account claim.
- **Risks / failure modes**:
  - Platform attestation is intentionally a no-op until its dedicated plan.

## Phase 2: Username read contract ✅
- **Goal**: let the registration screen check available discriminators.
- **Scope checklist**:
  - [x] Public `GET /api/v1/attester`.
  - [x] JWT-gated `POST /api/v1/usernames/available?version=v1`.
  - [x] Direct `Resources::UsernameOwnerOf` checks for `01..99`.
  - [x] Public `/usernames/search` placeholder with a valid empty response until the independent read service lands.
- **✅ Demo**: live Paseo Next v2 checks return `AVAILABLE`, `EXHAUSTED`, or `INVALID` with the shipping response shape.
- **Risks / failure modes**:
  - Prefix search remains empty by design; see `docs/plans/active/username-read.md`.

## Phase 3: Durable username registration ✅
- **Goal**: accept a signed registration and finalize it on People Chain.
- **Scope checklist**:
  - [x] `POST /api/v1/usernames` validates payloads and enforces `who == JWT subject`.
  - [x] Reservation outbox with `RESERVED → SUBMITTING → ASSIGNED | RETRY_AFTER | FAILED_TERMINAL`.
  - [x] Separate single-instance `identity-chain-writer` with lease, nonce discipline, retry, and reconciliation.
  - [x] Direct `PeopleLite.attest`, optionally wrapped in `Proxy.proxy`; no single-item `force_batch`.
  - [x] Deterministic failure decisions: retry, terminal failure, matching-owner reconciliation, and `AlreadyRegistered` handling.
- **✅ Demo**: on 2026-07-10, a generated real-signature payload registered `directzdxab.47` on Paseo Next v2. `UsernameOwnerOf`, `Resources.Consumers`, and the automatic allowance (`max_count=50`, `max_size=512000`) were verified.
- **Risks / failure modes**:
  - Real batching is deferred until measured throughput requires it.

## Phase 4: Mobile wire contracts ✅
- **Goal**: lock the current iOS and Android request/response shapes without requiring physical devices.
- **Scope checklist**:
  - [x] Auth header, challenge, token, refresh, and App Attest contracts.
  - [x] iOS, Play Integrity, and Android key-attestation body variants.
  - [x] Attester, availability, registration, optional DotNS, and search contracts.
  - [x] Android `status=ASSIGNED` search query compatibility.
- **✅ Demo**: `just check` passes the focused wire-contract and writer failure-path tests.
- **Risks / failure modes**:
  - Physical-device routing and UI behavior remain release validation, not Identity Service MVP implementation.

# Contracts (guardrails)
- Keep the shipping `/api/v1` paths and JSON field names.
- Keep `AUTH_ENABLED=false` as attestation-off mode while still verifying the account proof.
- Reject a registration beneficiary that differs from the JWT subject.
- Record `SUBMITTING` before awaiting finalization.
- Never blindly resubmit an unknown outcome; reconcile against `UsernameOwnerOf`.
- Keep `just check` green.

# Key decisions (decide early)
- All Identity Service MVP decisions are closed. Later service and release decisions live in their own plans.

# Testing
- `just check` for formatting, clippy, and workspace tests.
- Real-signature local-stack registration against live Paseo Next v2.
- Focused mobile wire-contract and writer failure-decision tests.

# Polish rounds (after MVP)

## Polish round 1: Production hardening
- Owned by a future hardening plan after the independent services are usable.
- Future acceptance gate: parity benchmarks and operational gates must pass before cutover.

# Later / Deferred
- See `docs/roadmap.md` for independent service plans and required release work.