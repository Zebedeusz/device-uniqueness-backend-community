# AGENTS.md

Operating contract for coding agents in this repo. Keep this file lean and operational —
project narrative, architecture, and specs live in `docs/` and are linked below. Read the relevant
doc before non-trivial work.

## Orientation

Identity Backend V2 — greenfield **Rust** rewrite of the Polkadot identity backend: an
**API-compatible** replacement (prove the device + register the username on People Chain, same client
contracts) built as isolated services, on track to **retire the legacy TypeScript backend**. Context
lives in docs, not here:

- **Current architecture, scope, service boundaries, naming, and data-flow invariants** → `docs/architecture.md`
- **Delivery index** → `docs/roadmap.md`; plans under `docs/plans/active/` and `docs/plans/done/`
- **Public API reference** → `docs/api-reference/`
- **Deploy / operations** → `docs/deploy.md`, `docs/runbook.md`

## Keeping docs current

The **code is the source of truth**; the docs must be kept in sync with it. When a change alters behavior, architecture, service boundaries, naming, or invariants, update the matching doc in the **same change**, not later:

- Architecture, scope, service boundaries, naming, or data-flow invariants → `docs/architecture.md`.
- Delivery status → `docs/roadmap.md` + the plan under `docs/plans/`.
- Commands, conventions, config, workspace layout, or gotchas → this file.
- **Public HTTP surface** (routes, request/response fields, status codes, examples) → the API reference is **generated from the code**, not hand-written. Annotate the handler with `#[utoipa::path(...)]` and its bodies with `#[derive(ToSchema)]`; register new handlers in the `paths(...)` list and new types in `components(schemas(...))` in `crates/<service>/src/openapi.rs`; then run `just openapi` and commit the regenerated `docs/api-reference/{openapi.json,index.html}` in the same change. Never hand-edit those two files (a test enforces this — see Gotchas). Edit the page **design** in `docs/api-reference/template.html` (the `<!--@NAV-->` / `<!--@ENDPOINTS-->` placeholders are filled by the generator).

If a change makes a doc wrong, fix the doc (or flag the drift) in the same change instead of leaving it stale.

## Workspace

Nine crates today (plans may add more independently-deployable service crates):

- `chain-types` — generated People Chain type surface (subxt codegen).
- `chain-client` — reconnecting People Chain connection + the chain-writer signing key (`WriterSigner`); product-agnostic transport shared by the services.
- `jwt-verify` — verify-only JWT validation (JWKS parsing, Ed25519 verification, claims); the cross-service auth contract consumed by every service that gates routes on identity-service tokens.
- `http-common` — shared axum primitives for the frozen legacy wire dialects (problem details / Hono text bodies / zod v4 messages, JWT extractor, rate limiter, health, middleware stack, fail-fast env helpers); consumed by dim-tickets and invite-tickets. The dialect bodies are golden-captured — change only with a fresh capture.
- `identity-service` — the identity service (lib + bins `identity-api` and `identity-chain-writer`).
- `username-indexer` — username indexer + search service.
- `dim-tickets` — game/personhood invitation-credential service (lib + bins `dim-tickets-api` and `dim-tickets-writer`).
- `invite-tickets` — synchronous invitation-credential claim service, the route the shipping apps call (lib + bins `invite-tickets-api` and `invite-tickets-pool`).
- `apidoc-gen` — dev-only tool (not deployed) that renders the committed API reference from the service crates' `#[utoipa::path]` annotations. Run via `just openapi`.

Each service's boundary, persistence, endpoints, and data-flow invariants are in `docs/architecture.md`.

## Commands

- `just check` — the full gate: fmt `--check` + `clippy -D warnings` + `cargo test --workspace`. CI runs the same three checks on push/PR; run it before declaring done.
- `just run` / `just run-writer` — run `identity-api` / `identity-chain-writer` locally.
- Single test: `cargo test -p identity-service <name>` (or `-p chain-types`, `-p username-indexer`).
- `just deny` — `cargo-deny` audit; **not** in `just check`, needs `cargo install cargo-deny`.
- `just openapi` — regenerate the committed API reference (`docs/api-reference/openapi.json` + `index.html`) from the `#[utoipa::path]` annotations. Run after any change to a route, request/response type, status code, or example.
- Local stack: `docker compose up` (Postgres + `identity-api` + `identity-chain-writer` + `username-indexer` + its own Postgres). All env vars in `.env.example`.

## Conventions (beyond rustfmt/clippy defaults)

- Workspace lints: `unsafe_code = "forbid"`, clippy `uninlined_format_args = warn` → inline format args (`"{v:?}"`, not `"{}", v`).
- **Errors:** domain enums via `thiserror` (`ConfigError`, `JwtError`, `InsertError`, …); HTTP maps through `http::error::AppError` (`impl IntoResponse`). `anyhow` only in the bin `main`s, never in library public APIs.
- **sqlx is runtime-checked** — queries use `sqlx::query(...)` with bound `$n` params; there are **no** `query!`/`query_as!` macros and **no `.sqlx` cache**. Do not add compile-time SQL macros or `cargo sqlx prepare` (a live-Postgres CI dependency is deliberately deferred).
- **Migrations auto-run** via `sqlx::migrate!("./migrations")` in `db::connect` on every boot (advisory-locked, safe across replicas). Don't run them by hand; add schema as new `crates/<service>/migrations/*.sql`.
- **Config is fail-fast** (`Config::from_env`): required vars (e.g. `DATABASE_URL`, `JWT_ED25519_SECRET`, `ATTESTER_PUBLIC_KEY`) have no defaults and abort startup if missing/malformed. Don't add fallbacks for them.
- Module layout: `foo.rs` + optional `foo/` children (no new `foo/mod.rs`); short `lib.rs` (docs + lints + `pub mod` + curated re-exports); doc every `pub` item.

## Configuration

All vars + dev defaults are in `.env.example` (compose auto-loads `.env`).
Non-obvious: `AUTH_ENABLED=false` is the current **M0 mode** — platform attestation is a **no-op**
(a JWT is still issued and the sr25519 account proof still verified); `ENFORCE_AUTH` toggles soft
(log-only) vs hard rejection.

## Gotchas / forbidden

- **Don't run migrations manually** — auto-applied on boot.
- **Don't scale `identity-chain-writer`** past one instance (nonce lane).
- **Don't add `.sqlx` / `query!` compile-time SQL** — runtime-checked by design.
- **Don't hand-edit `docs/api-reference/openapi.json` or `index.html`** — both are generated by `just openapi`; the `apidoc-gen` `committed_artifacts_are_in_sync` test (part of `just check`) fails if they're stale. Change the annotations (content) or `template.html` (design), then regenerate.
- **Run `just check` before declaring done** — the same three-check gate CI runs on push/PR. `cargo-deny` is separate (`just deny`).
- **Markdown-only pushes skip build/deploy** — docs/plans/README changes run `check` but don't rebuild the image or restart prod (CI uses a deployable-path allow-list).
- **Don't push or deploy without explicit consent.** Full deploy steps live in `docs/deploy.md`.
- **`identity-api` blocks on the People Chain RPC at startup** — no reachable RPC, no boot.
