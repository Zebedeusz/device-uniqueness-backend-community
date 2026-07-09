# AGENTS.md

This file gives coding agents the context they need to work in this repository.

## Project overview

Identity Backend V2 — greenfield **Rust** rewrite of the Polkadot identity
backend, scoped to **prove the device + register the username** on People Chain.
Full scope, rationale, and carve-outs live in `docs/design-doc.md`; the active
build plan is `docs/plans/active/v1-ship-plan.md`.

**Canonical username-registration spec** (ground-truth for the `/usernames` write path — Slice 4):
Alberto's "New JWT / Integrity / PoUD / Username Claim Logic" —
https://docs.google.com/document/d/1YQy0how-ItgThH5VfC1SR0yR6j_Xh9jLTOn35Oo9ubc
Covers Android md TEE attestation (`POST /auth/android/attestation`), PoUD (Android
`{androidId, widevineId}` / iOS DeviceCheck), the INSTANT / PAYMENT_REQUIRED / QUEUED decision flow,
the balance-priority queue (G1<10 … G4≥1000), QR-voucher bypass, and the `/usernames/payment-status`
+ `/registration/queue` endpoints. (To digest fully before implementing Slice 4.)

Cargo workspace, two crates:
- `chain-types` — canonical People Chain type surface (static subxt codegen).
- `identity-service` — domain lib plus two deployable binaries:
  - `identity-api` — N-replica axum HTTP server (auth handshake, JWKS, `/usernames`, `/attester`).
  - `identity-chain-writer` — single-instance worker draining the reservation outbox onto chain.

## Commands

- `just check` — the full gate: fmt `--check` + `clippy -D warnings` + `cargo test --workspace`. CI runs the same three checks on push/PR; run it before declaring done.
- `just run` / `just run-writer` — run `identity-api` / `identity-chain-writer` locally.
- Single test: `cargo test -p identity-service <name>` (or `-p chain-types`).
- `just deny` — `cargo-deny` audit; **not** in `just check`, needs `cargo install cargo-deny`.
- Local stack: `docker compose up` (Postgres + api + writer). All env vars in `.env.example`.

## Architecture / data flow

- **Two binaries, one Postgres.** Registration is an outbox: `POST /api/v1/usernames` inserts a `RESERVED` row in `username_reservations` and returns `202`; `identity-chain-writer` claims rows and submits the People Chain calls (`PeopleLite.attest` / `Resources.register_lite_person`), advancing `RESERVED → SUBMITTING → ASSIGNED | RETRY_AFTER | FAILED_TERMINAL`. **The DB row is source of truth; chain is reconciled to it.**
- **`identity-chain-writer` is single-instance by design** — one signer = one nonce lane. The `writer_lease` table is only a best-effort deploy-overlap guard; the chain nonce + outbox reconciliation is the real serializer. Never run two writers / never scale it.
- **Signing:** the writer signs as a least-privilege `Proxy` of a cold attester primary when `CHAIN_WRITER_PROXY_FOR` is set, else directly as its SURI.
- **`chain-types` is the only place chain types live.** Static codegen from vendored metadata at `crates/chain-types/metadata/people.scale`; online transport + signing live in `identity-service::chain`, never in `chain-types`. Regenerate metadata with the `subxt metadata …` command at the top of `crates/chain-types/src/lib.rs`.
- **HTTP** (`identity-service::http`): router in `http.rs` wrapped in a tower-http stack (request-id → trace → 30s timeout) with graceful shutdown. Health at `/livez`, `/readyz` (gates on a live Postgres `SELECT 1`), `/healthcheck`; JWKS at `/.well-known/jwks.json`. `AppState` = Postgres pool + `ChainClient` + `Jwt` + `Config`.

## Conventions (beyond rustfmt/clippy defaults)

- Workspace lints: `unsafe_code = "forbid"`, clippy `uninlined_format_args = warn` → inline format args (`"{v:?}"`, not `"{}", v`).
- **Errors:** domain enums via `thiserror` (`ConfigError`, `JwtError`, `InsertError`, …); HTTP maps through `http::error::AppError` (`impl IntoResponse`). `anyhow` only in the two bin `main`s, never in library public APIs.
- **sqlx is runtime-checked** — queries use `sqlx::query(...)` with bound `$n` params; there are **no** `query!`/`query_as!` macros and **no `.sqlx` cache**. Do not add compile-time SQL macros or `cargo sqlx prepare` (a live-Postgres CI dependency is deliberately deferred).
- **Migrations auto-run** via `sqlx::migrate!("./migrations")` in `db::connect` on every boot (advisory-locked, safe across replicas). Don't run them by hand; add schema as new `crates/identity-service/migrations/*.sql`.
- **Config is fail-fast** (`config::Config::from_env`): required `DATABASE_URL`, `JWT_ED25519_SECRET`, `ATTESTER_PUBLIC_KEY` have no defaults and abort startup if missing/malformed. Don't add fallbacks for them.
- Module layout: `foo.rs` + optional `foo/` children (no new `foo/mod.rs`); short `lib.rs` (docs + lints + `pub mod` + curated re-exports); doc every `pub` item.

## Configuration

All vars + dev defaults are in `.env.example` (compose auto-loads `.env`).
Non-obvious: `AUTH_ENABLED=false` is the current **M0 mode** — platform
attestation is a **no-op** (a JWT is still issued and the sr25519 account proof
still verified); `ENFORCE_AUTH` toggles soft (log-only) vs hard rejection.

## Deployment

Multi-stage `Dockerfile` builds both binaries (pure-Rust TLS, no OpenSSL);
`docker-compose.yml` runs Postgres + `identity-api` + `identity-chain-writer`, writer
pinned to `replicas: 1`. `identity-api` connects to the People Chain RPC at
startup (blocking) — it won't boot without reachable RPC. Do not push or deploy
without explicit consent.

## Gotchas / forbidden

- **Don't run migrations manually** — auto-applied on boot.
- **Don't scale `identity-chain-writer`** past one instance (nonce lane).
- **Don't add `.sqlx` / `query!` compile-time SQL** — runtime-checked by design.
- **`just check` is the only gate** (no CI); `cargo-deny` is separate (`just deny`).
ny`).
