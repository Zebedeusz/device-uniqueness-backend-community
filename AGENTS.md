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

Eleven crates today (plans may add more independently-deployable service crates):

- `ibv2` — **the single deployable binary**. Every service and worker is a *role* of this one
  process (`ibv2 --role identity-api`; `ibv2 --list-roles` prints the eight, `--list-merged-roles` the
  small topology's `all-in-one`). Process wiring only, no
  domain logic: each role module holds that service's former `main` body and enters the service
  crate through its library `routes()` / `run()`. Also serves `--healthcheck` (a GET on this
  container's own `/readyz`, so an image needs no `curl`). Adding a role means adding it to
  `ROLES` **and** to compose and the chart — `scripts/verify_role_split.sh` fails
  if those disagree. Also holds `src/routes.rs`, **the route table**: the ownership map that
  `gateway/Caddyfile` proxies, in Rust, which `--role all-in-one` serves so a consumer can run the
  whole API with no edge. `all-in-one` is accepted by `--role` but deliberately **absent from
  `--list-roles`** (it holds every secret in one process), so the gate rejects it in any manifest.
  **Two topologies**: standard (eight workloads) and small (`all-in-one` + the three workers). They are
  mutually exclusive, the small one needs `global.allInOne.acknowledgedSecretUnion: true`, and the
  real environments stay standard — `docs/architecture.md` "Deployment topologies" has the threat
  model, `docs/deploy.md` "Choosing a topology" the operator view.
  See `docs/plans/active/one-image-one-binary.md`.
- `chain-types` — generated People Chain type surface (subxt codegen).
- `chain-client` — reconnecting People Chain connection + the chain-writer signing key (`WriterSigner`); product-agnostic transport shared by the services.
- `jwt-verify` — verify-only JWT validation (JWKS parsing, Ed25519 verification, claims); the cross-service auth contract consumed by every service that gates routes on identity-service tokens.
- `http-common` — shared axum primitives for the frozen legacy wire dialects (problem details / Hono text bodies / zod v4 messages, JWT extractor, rate limiter, health, middleware stack, fail-fast env helpers); consumed by invite-tickets. The dialect bodies are golden-captured — change only with a fresh capture. Also holds the two things **every** process installs: the metrics exporter (`metrics::spawn`) and the log subscriber (`telemetry::init`).
- `identity-service` — the identity service (lib + the `voucher-mint` CLI; roles `identity-api`, `identity-chain-writer`, and the `registration-queue` advancer — with `QUEUE_ENABLED` on, its promotion is the only queue exit: down, claims park as `QUEUED` and the writer raises a stranded-queue warning).
- `username-indexer` — username indexer + search service.
- `invite-tickets` — synchronous invitation-credential claim service, the route the shipping apps call (lib; roles `invite-tickets-api` and `invite-tickets-pool`).
- `turn` — stateless TURN credential issuer (coturn REST-API HMAC construction over a relay-shared secret; lib; role `turn-api`). No DB, no chain.
- `notifications` — thin `/api/v1/notify` relay (verify-only Ed25519 JWT, stateless, DB-free, per-subject rate limited; role `notify-relay`) with optional iOS APNs + Android FCM providers.
- `apidoc-gen` — dev-only tool (not deployed) that renders the committed API reference from the service crates' `#[utoipa::path]` annotations. Run via `just openapi`.
- `routegen` — dev-only tool (not deployed) that renders the committed edge configs from `ibv2`'s route table. Run via `just routes`.

Each service's boundary, persistence, endpoints, and data-flow invariants are in `docs/architecture.md`.

## Commands

- `just check` — the fast offline gate: fmt `--check` + `clippy -D warnings` + `cargo test --workspace`.
- `just test-live-db` (also `just test-live`) — the deterministic Postgres gate: 13 suites / 28 ignored tests against a per-run isolated Compose project. CI runs it after `just check`; run both before declaring done.
- `just test-live-chain` — optional Postgres + live People Chain suites; external RPC availability keeps it outside the merge/release gate.
- `just test-live-providers` — optional credentialed APNs/FCM smokes; runs only the providers configured in the environment and fails if neither is configured.
- `just coverage-db` — canonical deterministic coverage (offline + Postgres). `just coverage-full` additionally merges the optional live-chain suites.
- `just run` / `just run-writer` / `just run-notify` — run a role locally (`cargo run -p ibv2 -- --role <name>`).
- Single test: `cargo test -p identity-service <name>` (or `-p chain-types`, `-p username-indexer`).
- `just deny` — `cargo-deny` audit; **not** in `just check`, needs `cargo install cargo-deny`.
- `just routes` — regenerate the committed edge configs (the `(routes)` snippet in `gateway/Caddyfile`, the `routes:` list in `helm/values.yaml`) from `crates/ibv2/src/routes/table.rs`.
- `just openapi` — regenerate the committed API reference (`docs/api-reference/openapi.json` + `index.html`) from the `#[utoipa::path]` annotations. Run after any change to a route, request/response type, status code, or example.
- Images: **ONE image, `$IMAGE_REPO:$IMAGE_TAG`** (`docker-bake.hcl`, one target).
  `docker buildx bake all` builds it from one `cargo build -p ibv2`. What makes a container a given
  service is its role — compose `command: ["--role", "<svc>"]`, chart `args:` — never a different
  image. The image carries `ENTRYPOINT ["ibv2"]` and **no `CMD`**, so an argument-less container
  fails loudly instead of defaulting into somebody's service.
  `scripts/verify_role_split.sh` (in `check.yml`) fails if a compose service or chart workload names
  a role `ibv2 --list-roles` does not accept, if a role is claimed by more than one or by none, if a
  service name and its role disagree, if the services drift onto different image references, or if a
  `CMD` reappears in the Dockerfile.
- Deployment gates, all in `check.yml` and reachable as `just verify-config` /
  `just verify-routes`: `verify_compose_boundaries.sh` (compose secret allowlists),
  `verify_role_split.sh` (one image, every workload's `--role`, + the release exporter shape),
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
- **Config is fail-fast** (`Config::from_env`): required vars (e.g. `IDENTITY_DATABASE_URL`, `JWT_ED25519_SECRET`, `ATTESTER_ACCOUNT`) have no defaults and abort startup if missing/malformed. Don't add fallbacks for them.
- **Per-service config keys are namespaced.** The database URLs are `IDENTITY_` / `INDEXER_` / `INVITE_TICKETS_DATABASE_URL` and the rate limits `INVITE_TICKETS_` / `TURN_RATE_LIMIT[_WINDOW_SECS]` — a bare `DATABASE_URL` used to name several different Postgres instances, so `all-in-one` would have wired services to each other's databases. The bare names are still read for one release with a `WARN` (`http_common::config::renamed*`); `verify_compose_boundaries.sh` asserts each credential reaches only its own services and that nothing is left on a bare name. `BIND_ADDR` and `PEOPLE_RPC_URL` stay shared — one listener, one chain.
- Module layout: `foo.rs` + optional `foo/` children (no new `foo/mod.rs`); short `lib.rs` (docs + lints + `pub mod` + curated re-exports); doc every `pub` item.
- **Every role starts the same two lines**: `http_common::telemetry::init("<service>")` then `http_common::metrics::spawn("<service>")`, as the first two lines of its `run()`, with its OWN service name — both are process-global and `metrics::spawn` sets `service` as a global label, so hoisting either into `main` would relabel every scrape target. Don't build a subscriber in a role. Logs are aggregated, so prefer structured fields (`tracing::warn!(reservation_id, %error, "…")`) over interpolated message text — the field is queryable, the sentence is not. Don't add the service/environment name as a field: the log shipper labels those.

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
- **Don't hand-edit the route table between the `generated:route-table` markers** in `gateway/Caddyfile` or `helm/values.yaml`. Both regions are rendered by `just routes` from the ONE table in `crates/ibv2/src/routes/table.rs`, which is also what `--role all-in-one` serves; `routegen`'s `committed_artifacts_are_in_sync` test (part of `just check`) fails if either is stale. Change the table, run `just routes`, and commit all three together. `just verify-routes` then proves the two renderings still mean the same thing to real Caddy and real Traefik.
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
