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
- **Delivery status** → the plans themselves: `docs/plans/active/` (in flight) and `docs/plans/done/` (shipped). **Remaining scope + legacy-retirement bar** → `docs/architecture.md`
- **Public API reference** → `docs/api-reference/`
- **Deploy / operations** → `docs/deploy.md`, `docs/runbook.md`

## Keeping docs current

The **code is the source of truth**; the docs must be kept in sync with it. When a change alters behavior, architecture, service boundaries, naming, or invariants, update the matching doc in the **same change**, not later:

- Architecture, scope, service boundaries, naming, or data-flow invariants → `docs/architecture.md`.
  It carries **no delivery status** (no "exists"/"pending"/"in flight", no plan links, no dated
  demo notes) — it describes the system as designed, so only a design change touches it.
- Delivery status → the plan under `docs/plans/` (move it `active/` → `done/` when it ships). Remaining/not-yet-started scope + the legacy-retirement bar → `docs/architecture.md`.
- Commands, conventions, config, workspace layout, or gotchas → this file.
- **Public HTTP surface** (routes, request/response fields, status codes, examples) → the API reference is **generated from the code**, not hand-written. Annotate the handler with `#[utoipa::path(...)]` and its bodies with `#[derive(ToSchema)]`; register new handlers in the `paths(...)` list and new types in `components(schemas(...))` in `crates/<service>/src/openapi.rs`; then run `just openapi` and commit the regenerated `docs/api-reference/{openapi.json,index.html}` in the same change. Never hand-edit those two files (a test enforces this — see Gotchas). Edit the page **design** in `docs/api-reference/template.html` (the `<!--@NAV-->` / `<!--@ENDPOINTS-->` placeholders are filled by the generator).

If a change makes a doc wrong, fix the doc (or flag the drift) in the same change instead of leaving it stale.

## Planning

Each substantial slice gets its own ship-first plan under `docs/plans/` (`active/` while in flight, `done/` once shipped), following the vendored `ship-first-plan` skill's template (`.claude/skills/ship-first-plan/SKILL.md` — Goals, Non-goals, demoable phases with ✅ demos, Testing, Later):

- **One independently deployable service or bounded capability per plan** — so one owner can deliver it without editing another service's plan.
- **Correctness and security belong in every MVP; measured optimization and cross-service hardening wait until before cutover.**

## Workspace

Ten crates today (plans may add more independently-deployable service crates):

- `chain-types` — generated People Chain type surface (subxt codegen).
- `chain-client` — reconnecting People Chain connection + the chain-writer signing key (`WriterSigner`); product-agnostic transport shared by the services.
- `jwt-verify` — verify-only JWT validation (JWKS parsing, Ed25519 verification, claims); the cross-service auth contract consumed by every service that gates routes on identity-service tokens.
- `http-common` — shared axum primitives for the frozen legacy wire dialects (problem details / Hono text bodies / zod v4 messages, JWT extractor, rate limiter, health, middleware stack, fail-fast env helpers); consumed by dim-tickets and invite-tickets. The dialect bodies are golden-captured — change only with a fresh capture.
- `identity-service` — the identity service (lib + bins `identity-api`, `identity-chain-writer`, and the `registration-queue` advancer — with `QUEUE_ENABLED` on, its promotion is the only queue exit: down, claims park as `QUEUED` and the writer raises a stranded-queue warning).
- `username-indexer` — username indexer + search service.
- `dim-tickets` — game/personhood invitation-credential service (lib + bins `dim-tickets-api` and `dim-tickets-writer`).
- `invite-tickets` — synchronous invitation-credential claim service, the route the shipping apps call (lib + bins `invite-tickets-api` and `invite-tickets-pool`).
- `turn` — stateless TURN credential issuer (coturn REST-API HMAC construction over a relay-shared secret; lib + bin `turn-api`). No DB, no chain.
- `notifications` — thin `/api/v1/notify` relay (verify-only Ed25519 JWT, stateless, DB-free, per-subject rate limited; bin `notify-relay`) with optional iOS APNs + Android FCM providers.
- `apidoc-gen` — dev-only tool (not deployed) that renders the committed API reference from the service crates' `#[utoipa::path]` annotations. Run via `just openapi`.

Each service's boundary, persistence, endpoints, and data-flow invariants are in `docs/architecture.md`.

## Commands

- `just check` — the full gate: fmt `--check` + `clippy -D warnings` + `cargo test --workspace`. CI runs the same three checks on push/PR; run it before declaring done.
- `just run` / `just run-writer` — run `identity-api` / `identity-chain-writer` locally.
- Single test: `cargo test -p identity-service <name>` (or `-p chain-types`, `-p username-indexer`).
- `just deny` — `cargo-deny` audit; **not** in `just check`, needs `cargo install cargo-deny`.
- `just openapi` — regenerate the committed API reference (`docs/api-reference/openapi.json` + `index.html`) from the `#[utoipa::path]` annotations. Run after any change to a route, request/response type, status code, or example.
- Local stack: `docker network create ibv2-edge` once, then `docker compose up`
  (Postgres + `identity-api` + `identity-chain-writer` + `username-indexer` + its own Postgres).
  All env vars in `.env.example`. Nothing publishes a host port: add
  `-f docker-compose.yml -f docker-compose.debug.yml` to republish them on `127.0.0.1`.
- Edge: its own project, so environment restarts never touch it. Locally:
  `GATEWAY_ADDRESS=http://identity.dotspark.app EDGE_HTTP_PORT=8000 EDGE_HTTPS_PORT=8443 docker compose -f gateway/docker-compose.yml -p edge up -d`,
  then curl `127.0.0.1:8000` with a `Host:` header.
- Metrics: also its own project. `docker network create ibv2-metrics` once, then
  `docker compose -f observability/docker-compose.yml -p observability up -d`
  (Prometheus UI on `127.0.0.1:9091`). Every process exports on 9090 over that
  network only; scrape targets live in `observability/prometheus.yml`.

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
- **Don't scale `identity-chain-writer`** past one instance (nonce lane). `registration-queue` is also single-instance (its lease guards promotion order and doubles as the queue's liveness signal).
- **Don't add `.sqlx` / `query!` compile-time SQL** — runtime-checked by design.
- **Don't hand-edit `docs/api-reference/openapi.json` or `index.html`** — both are generated by `just openapi`; the `apidoc-gen` `committed_artifacts_are_in_sync` test (part of `just check`) fails if they're stale. Change the annotations (content) or `template.html` (design), then regenerate.
- **Run `just check` before declaring done** — the same three-check gate CI runs on push/PR. `cargo-deny` is separate (`just deny`).
- **Markdown-only pushes skip build/deploy** — docs/plans/README changes run `check` but don't rebuild the image or restart prod (CI uses a deployable-path allow-list).
- **Don't add `ports:` to `docker-compose.yml`** — the edge container is the only thing that
  publishes on the host (enforced by `scripts/verify_compose_boundaries.sh`). Local port access
  goes in `docker-compose.debug.yml`, loopback-only.
- **Don't push or deploy without explicit consent.** Full deploy steps live in `docs/deploy.md`.
- **`identity-api` blocks on the People Chain RPC at startup** — no reachable RPC, no boot.
