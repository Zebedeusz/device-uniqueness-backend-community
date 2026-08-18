# Single-URL gateway — ship-first plan

> **Superseded in part (2026-08-03)** by `previewnet-env.md` Phase 2: the route table is still
> this committed `gateway/Caddyfile`, but it is now loaded by an **edge container** in its own
> compose project (`-p edge`) that owns 80/443 — not a host-installed Caddy — and upstreams are
> container aliases (`device-attestation-api-paseo-next-v2:8080`) rather than `127.0.0.1:808x`. No
> application or Postgres container publishes a host port any more. Read the notes below as the
> history of this slice, not the current topology.
>
> The **route-by-route cutover lane** (`/api/v1/schemas`, `/api/v1/subscriptions`, `/admin`,
> `/debug/*` → `LEGACY_BACKEND_ORIGIN`) was **removed** on 2026-08-03: no legacy origin was ever
> configured, so all four already answered 404 through `device-attestation-api`'s fallthrough — deleting the
> blocks is behaviour-identical, and Phase 2 below is dropped with it.

> Working agreement: keep phases small; each ends in a runnable demo. Check boxes `[ ]`→`[x]` with
> dates. The plan file is the single source of truth for this slice.

## What ships

One public URL serving the whole migration, with the route table as a **committed, validated
artifact** instead of hand-typed doc text: `gateway/Caddyfile` is the single source of truth,
used identically by the host Caddy in production (env-placeholder defaults match the current
prod layout, so it drops in with zero configuration) and by a `gateway` compose service for the
local single-URL topology. Plus the **route-by-route cutover lane**: every legacy surface with
no Rust owner yet forwards to a configurable legacy origin, so the apps' domain can point at
this gateway before the migration is complete.

## Route table (the frozen ownership map)

| Path | Upstream | Status |
|---|---|---|
| `/docs*` | static API reference (`file_server`) | committed artifact |
| `GET /api/v1/usernames*` (search; bare-path GET and the removed `/{username}` lookup both 404; **except `/payment-status`**, carved out to device-attestation-api — the eligibility slice's poll) | `username-indexer` (:8081) | migrated |
| `/api/v1/dim-ticket*` | device-attestation-api catch-all (frozen 404) | **out of V2 scope** — no client consumer; the service was removed (see `docs/plans/done/dim-tickets.md`) |
| `/api/v1/invitation-ticket/*` | `invite-tickets-api` (:8083) | migrated |
| `/api/v1/turn/*` | `turn-api` (:8084) | migrated |
| `/api/v1/notify*` | `notify-relay` (:8085) | migrated |
| `/api/v1/poc*` | `username-indexer` (:8081) | migrated (2026-07-29; gates that service's own public search read, off by default) |
| `/api/v1/schemas*` | **legacy origin** | unowned — needs an in/out ruling |
| `/api/v1/subscriptions*` | **legacy origin** | unowned — needs an in/out ruling |
| `/admin*`, `/debug/*` | **legacy origin** | legacy ops surfaces; die with legacy |
| everything else (auth, attester, usernames write + availability + root OPTIONS preflight, JWKS, health) | `device-attestation-api` (:8080) | migrated |

The legacy origin is `{$LEGACY_BACKEND_ORIGIN}`; it **defaults to the device-attestation-api upstream**,
so an undeployed legacy backend degrades to a 404 from device-attestation-api rather than a broken
gateway. During cutover it points at the deployed legacy backend; at legacy shutdown the whole
legacy block is deleted. Deleting one `handle` as its Rust owner ships is the per-route
cutover.

## Phases

- [x] (2026-07-22) **Phase 1 — the committed gateway.** `gateway/Caddyfile` (env-parameterized, prod-default
  values); compose `gateway` service (`caddy:2-alpine`, :8000, mounts the Caddyfile + the API
  reference, no secrets — enforced by `scripts/verify_compose_boundaries.sh`); deploy.md
  rewritten to install the committed file instead of transcribing config; CI deploy reloads
  Caddy from the repo copy after each pull (validate-then-reload, so a bad file never replaces
  a good running config). ✅ Demo (2026-07-22): `caddy validate` green on the committed file
  (caddy:2-alpine); compose config + boundary script green (gateway secret-forbid checks
  included); live routing smoke through the gateway container over the prod-default upstreams —
  `/api/v1/turn/issue` reached a live `turn-api` (401 without JWT, 201 with), `/docs/` served
  the committed reference, and both the default and legacy lanes fell through to the (down)
  device-attestation-api upstream as 502s, proving the handle order.
- [~] **Phase 2 — cutover rehearsal — dropped 2026-08-03** (the lane was removed; see the note above). Point `LEGACY_BACKEND_ORIGIN` at a
  reachable legacy deployment and verify an unmigrated route (e.g. `/api/v1/schemas`) round-trips
  through the single URL; then the apps' domain can move. Needs a live legacy origin — rolls
  into the migration-wide cutover drill.

## Risks / notes

- The gateway must never hold secrets: it terminates TLS and routes; every app service keeps its
  own env allowlist. The boundary script now asserts this for the compose gateway.
- Forwarding `https://` legacy origins needs the Host header rewritten to the upstream
  (`header_up Host {upstream_hostport}`) — baked into the legacy snippet.
- `/admin*` and `/debug/*` forward to legacy only when a legacy origin is configured (the
  default sends them to device-attestation-api, which 404s). They are never owned by a Rust service.
- Compose still publishes per-service host ports (8080–8084) for direct debugging; the gateway
  (:8000 dev, :443 prod) is the only surface clients should use.
