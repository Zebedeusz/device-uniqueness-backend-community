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
- `http-common` — shared axum primitives for the frozen legacy wire dialects (problem details / Hono text bodies / zod v4 messages, JWT extractor, rate limiter, health, middleware stack, fail-fast env helpers); consumed by dim-tickets and invite-tickets. The dialect bodies are golden-captured — change only with a fresh capture. Also holds the two things **every** process installs: the metrics exporter (`metrics::spawn`) and the log subscriber (`telemetry::init`).
- `identity-service` — the identity service (lib + bins `identity-api`, `identity-chain-writer`, and the `registration-queue` advancer — with `QUEUE_ENABLED` on, its promotion is the only queue exit: down, claims park as `QUEUED` and the writer raises a stranded-queue warning).
- `username-indexer` — username indexer + search service.
- `dim-tickets` — game/personhood invitation-credential service (lib + bins `dim-tickets-api` and `dim-tickets-writer`).
- `invite-tickets` — synchronous invitation-credential claim service, the route the shipping apps call (lib + bins `invite-tickets-api` and `invite-tickets-pool`).
- `turn` — stateless TURN credential issuer (coturn REST-API HMAC construction over a relay-shared secret; lib + bin `turn-api`). No DB, no chain.
- `notifications` — thin `/api/v1/notify` relay (verify-only Ed25519 JWT, stateless, DB-free, per-subject rate limited; bin `notify-relay`) with optional iOS APNs + Android FCM providers.
- `apidoc-gen` — dev-only tool (not deployed) that renders the committed API reference from the service crates' `#[utoipa::path]` annotations. Run via `just openapi`.

Each service's boundary, persistence, endpoints, and data-flow invariants are in `docs/architecture.md`.

## Commands

- `just check` — the fast offline gate: fmt `--check` + `clippy -D warnings` + `cargo test --workspace`.
- `just test-live-db` (also `just test-live`) — the deterministic Postgres gate: 14 suites / 38 ignored tests against a per-run isolated Compose project. CI runs it after `just check`; run both before declaring done.
- `just test-live-chain` — optional Postgres + live People Chain suites; external RPC availability keeps it outside the merge/release gate.
- `just test-live-providers` — optional credentialed APNs/FCM smokes; runs only the providers configured in the environment and fails if neither is configured.
- `just coverage-db` — canonical deterministic coverage (offline + Postgres). `just coverage-full` additionally merges the optional live-chain suites.
- `just run` / `just run-writer` — run `identity-api` / `identity-chain-writer` locally.
- Single test: `cargo test -p identity-service <name>` (or `-p chain-types`, `-p username-indexer`).
- `just deny` — `cargo-deny` audit; **not** in `just check`, needs `cargo install cargo-deny`.
- `just openapi` — regenerate the committed API reference (`docs/api-reference/openapi.json` + `index.html`) from the `#[utoipa::path]` annotations. Run after any change to a route, request/response type, status code, or example.
- Images: **one per service, one binary each** (`docker-bake.hcl`). `docker buildx bake all`
  builds all ten off a single `cargo build` — the Dockerfile's `builder` stage is shared and each
  service stage copies just its own binary. Never add a stage that bundles several. All ten carry
  one tag (`$IMAGE_REGISTRY/<service>:$IMAGE_TAG`); `scripts/verify_image_split.sh` (in `check.yml`)
  fails if a service's stage, compose `build.target`, image repo, or tag drifts out of line, or if
  a compose `command:` reappears to override an image's `CMD`.
- Deployment gates, all in `check.yml` and reachable as `just verify-config` /
  `just verify-routes`: `verify_compose_boundaries.sh` (compose secret allowlists),
  `verify_image_split.sh` (one service/image/binary/tag + the release exporter shape),
  `verify_helm_render.sh` (the same boundaries on the rendered chart, env-key parity with
  compose, probe budgets), `verify_route_parity_live.sh` (real Caddy vs real Traefik).
  **Kubernetes deployment** — chart in `helm/`, status and sequencing in
  `docs/deploy.md` and `docs/plans/active/kubernetes-helm-deploy.md`.
- Releasing: open a PR titled **`chore(release): vX.Y.Z`** carrying the `Cargo.toml` version bump
  and the `CHANGELOG.md` section; merging it tags the merge commit and runs the whole release
  (`release-on-merge.yml` → `release.yml`), while `deploy.yml` rolls the host from the same merge.
  Manual `git push --tags` still works. See `docs/deploy.md` — "Cutting a release".
- Local stack: `docker network create ibv2-edge` once, then `docker compose up`
  (Postgres + `identity-api` + `identity-chain-writer` + `username-indexer` + its own Postgres).
  All env vars in `.env.example`. Nothing publishes a host port: add
  `-f docker-compose.yml -f docker-compose.debug.yml` to republish them on `127.0.0.1`.
- Edge: its own project, so environment restarts never touch it. Locally:
  `GATEWAY_ADDRESS=http://identity.dotspark.app EDGE_HTTP_PORT=8000 EDGE_HTTPS_PORT=8443 docker compose -f gateway/docker-compose.yml -p edge up -d`,
  then curl `127.0.0.1:8000` with a `Host:` header.
- Metrics and logs: their own compose project. `docker network create ibv2-metrics` once, then
  `docker compose -f observability/docker-compose.yml -p observability up -d`
  (Prometheus + Loki + Alloy + Grafana). **Grafana on `127.0.0.1:3000` is the
  entry point** (dashboard `ibv2-overview`, anonymous read-only); Prometheus UI
  on `127.0.0.1:9091`, Alloy's on `127.0.0.1:12345`. Every process exports
  metrics on 9090 over that network only; scrape targets live in
  `observability/prometheus.yml`. Logs need no per-service wiring: Alloy
  discovers containers through a read-only Docker socket and labels them from
  Docker metadata, so a service only has to emit `LOG_FORMAT=json`.

## Conventions (beyond rustfmt/clippy defaults)

- Workspace lints: `unsafe_code = "forbid"`, clippy `uninlined_format_args = warn` → inline format args (`"{v:?}"`, not `"{}", v`).
- **Errors:** domain enums via `thiserror` (`ConfigError`, `JwtError`, `InsertError`, …); HTTP maps through `http::error::AppError` (`impl IntoResponse`). `anyhow` only in the bin `main`s, never in library public APIs.
- **sqlx is runtime-checked** — queries use `sqlx::query(...)` with bound `$n` params; there are **no** `query!`/`query_as!` macros and **no `.sqlx` cache**. Do not add compile-time SQL macros or `cargo sqlx prepare`; deterministic runtime database suites run separately in CI.
- **Migrations auto-run** via `sqlx::migrate!("./migrations")` in `db::connect` on every boot (advisory-locked, safe across replicas). Don't run them by hand; add schema as new `crates/<service>/migrations/*.sql`.
- **Config is fail-fast** (`Config::from_env`): required vars (e.g. `DATABASE_URL`, `JWT_ED25519_SECRET`, `ATTESTER_ACCOUNT`) have no defaults and abort startup if missing/malformed. Don't add fallbacks for them.
- Module layout: `foo.rs` + optional `foo/` children (no new `foo/mod.rs`); short `lib.rs` (docs + lints + `pub mod` + curated re-exports); doc every `pub` item.
- **Every deployed bin starts the same two lines**: `http_common::telemetry::init("<service>")` then `http_common::metrics::spawn("<service>")`. Don't build a subscriber in a bin. Logs are aggregated, so prefer structured fields (`tracing::warn!(reservation_id, %error, "…")`) over interpolated message text — the field is queryable, the sentence is not. Don't add the service/environment name as a field: the log shipper labels those.

## Configuration

All vars + dev defaults are in `.env.example` (compose auto-loads `.env`).
Non-obvious: `AUTH_ENABLED=false` is the current **M0 mode** — platform attestation is a **no-op**
(a JWT is still issued and the sr25519 account proof still verified); `ENFORCE_AUTH` toggles soft
(log-only) vs hard rejection. `LOG_FORMAT` defaults to `text` in the binaries and `json` in
compose — containers ship to Loki, `just run` is read by a human.

## Gotchas / forbidden

- **Don't run migrations manually** — auto-applied on boot.
- **Don't scale `identity-chain-writer`** past one instance (nonce lane). `registration-queue` is also single-instance (its lease guards promotion order and doubles as the queue's liveness signal).
- **Don't add `.sqlx` / `query!` compile-time SQL** — runtime-checked by design.
- **Don't hand-edit `docs/api-reference/openapi.json` or `index.html`** — both are generated by `just openapi`; the `apidoc-gen` `committed_artifacts_are_in_sync` test (part of `just check`) fails if they're stale. Change the annotations (content) or `template.html` (design), then regenerate.
- **Run `just check` and `just test-live-db` before declaring done** — CI runs both on push/PR. `cargo-deny` is separate (`just deny`).
- **Markdown-only pushes skip build/deploy** — docs/plans/README changes run `check` but don't rebuild the image or restart prod (CI uses a deployable-path allow-list).
- **Don't add `ports:` to `docker-compose.yml`** — the edge container is the only thing that
  publishes on the host (enforced by `scripts/verify_compose_boundaries.sh`). Local port access
  goes in `docker-compose.debug.yml`, loopback-only.
- **Don't widen the observability project's ports past `127.0.0.1`, and don't edit the Grafana
  dashboard in the UI** — Grafana/Prometheus/Alloy are tunnel-only (same script enforces it), and
  the dashboard is provisioned read-only from
  `observability/grafana/dashboards/ibv2-overview.json`. Change the file, restart Grafana.
- **Don't push or deploy without explicit consent.** Full deploy steps live in `docs/deploy.md`.
- **`identity-api` blocks on the People Chain RPC at startup** — no reachable RPC, no boot.
