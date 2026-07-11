# AGENTS.md

Operating contract for coding agents in this repo. Keep this file lean and operational —
project narrative, architecture, and specs live in `docs/` and are linked below. Read the relevant
doc before non-trivial work.

## Orientation

Identity Backend V2 — greenfield **Rust** rewrite of the Polkadot identity backend: an
**API-compatible** replacement (prove the device + register the username on People Chain, same client
contracts) built as isolated services, on track to **retire the legacy TypeScript backend**. Context
lives in docs, not here:

- **Scope, rationale, carve-outs** → `docs/design-doc.md` (a living design doc — expect it to change).
- **Architecture, service boundaries, naming, data-flow invariants** → `docs/architecture.md`
- **Delivery index** → `docs/roadmap.md`; plans under `docs/plans/active/` and `docs/plans/done/`
- **Deploy / operations** → `docs/deploy.md`, `docs/runbook.md`

## Keeping docs current

The **code is the source of truth**; the docs must be kept in sync with it. When a change alters behavior, architecture, service boundaries, naming, or invariants, update the matching doc in the **same change**, not later:

- Architecture, service boundaries, naming, or data-flow invariants → `docs/architecture.md`.
- Scope / carve-outs → `docs/design-doc.md`; delivery status → `docs/roadmap.md` + the plan under `docs/plans/`.
- Commands, conventions, config, workspace layout, or gotchas → this file.

If a change makes a doc wrong, fix the doc (or flag the drift) in the same change instead of leaving it stale.

## Workspace

Three crates today (plans may add more independently-deployable service crates):

- `chain-types` — generated People Chain type surface (subxt codegen).
- `identity-service` — the identity service (lib + bins `identity-api` and `identity-chain-writer`).
- `username-indexer` — username indexer + search service.

Each service's boundary, persistence, endpoints, and data-flow invariants are in `docs/architecture.md`.

## Commands

- `just check` — the full gate: fmt `--check` + `clippy -D warnings` + `cargo test --workspace`. CI runs the same three checks on push/PR; run it before declaring done.
- `just run` / `just run-writer` — run `identity-api` / `identity-chain-writer` locally.
- Single test: `cargo test -p identity-service <name>` (or `-p chain-types`, `-p username-indexer`).
- `just deny` — `cargo-deny` audit; **not** in `just check`, needs `cargo install cargo-deny`.
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
- **Run `just check` before declaring done** — the same three-check gate CI runs on push/PR. `cargo-deny` is separate (`just deny`).
- **Don't push or deploy without explicit consent.** Full deploy steps live in `docs/deploy.md`.
- **`identity-api` blocks on the People Chain RPC at startup** — no reachable RPC, no boot.
