# TURN credential issuer — ship-first plan

> Working agreement: keep phases small; each ends in a runnable demo. Check boxes `[ ]`→`[x]` with
> dates. The plan file is the single source of truth for this slice.

## What ships

The legacy `POST /api/v1/turn/issue` route (short-lived TURN credentials for WebRTC ICE
negotiation) as an independently deployable **stateless** service crate `turn` (bin `turn-api`),
per `docs/architecture.md` ("notifications / turn / invites → independent thin services").

## The frozen wire contract

Captured from the real legacy route (fixtures + provenance in `crates/turn/tests/fixtures/`):

- `POST /api/v1/turn/issue`, JWT-gated (`verifyJwt` + the `authActions` rate bucket in legacy
  `mod.ts` — the same shared middleware as dim/invite, so the 401 trio / 429 / 400
  problem-details / Hono text 400 / Hono text 404 dialects all apply via `http-common`).
- Request body: `{ regionHint?: string | null }` — accepted and **ignored** (reserved).
- **201** (not 200): `{ servers: [string], username, password, ttl }` where
  `username = "{unixExpiry}:{hex(8 random bytes)}"` (expiry = now + TTL),
  `password = base64(HMAC_<algo>(secret, username))` (the coturn REST-API construction,
  algo configurable, default SHA1), `ttl` = configured TTL in whole seconds, `servers` =
  configured ICE server URL list.
- No route-specific error bodies: anything unexpected is the global hook's opaque 500.

Config (fail-fast, mirroring legacy env names): `TURN_SECRET` (base64, required),
`TURN_AUTH_ALGORITHM` (`SHA1|SHA256|SHA384|SHA512`, default `SHA1`), `TURN_TTL_SECS`
(default 1800; legacy `TURN_TTL` took Effect duration strings — deployment-config format, not
wire contract), `TURN_REALM` (required + validated like legacy, reserved — not on the wire),
`ICE_SERVERS` (comma-separated URLs, default empty), JWT key material + rate-limit knobs as in
the other verify-only services.

## Phases

- [x] (2026-07-22) **Phase 1 — crate + frozen route + golden fixtures.** `crates/turn`: `credentials.rs`
  (HMAC issue + a verify helper), `config.rs`, `http.rs` (route on `http-common` scaffolding,
  health with an always-ready probe — there is nothing to gate on), `openapi.rs`; golden capture
  from the legacy repo (both SHA1 and SHA256 apps to pin the algo dimension); fixture replay
  (offline router replay for every deterministic case; 201s verified by **recomputing the HMAC
  over the captured username with the capture secret** — cryptographic parity, not just shape).
  ✅ Demo (2026-07-22): 18 fixtures captured and replayed green (12 deterministic byte-matches
  through the router; 6 credential cases verified cryptographically on both sides, SHA1 + SHA256);
  live `curl` against the running bin with a real EdDSA JWT: 201 minting credentials that verify
  relay-side under the shared secret, the missing-content-type 201 quirk, 400 problem details,
  Hono text 400/404. Notable captured quirk: missing JSON content-type (or no body at all) is a
  **201** — the legacy handler optional-chained where dim/invite crashed into a 500.
- [x] (2026-07-22) **Phase 2 — deploy wiring.** Dockerfile bin, compose service (`turn-api`
  :8084, no DB), Caddy route `/api/v1/turn/*` in `deploy.md`, `.env.example`, runbook notes
  (secret rotation = rotate relay + issuer together; relay-mismatch failure mode), apidoc-gen
  registration + regenerated reference (tag "TURN"). ✅ Demo (2026-07-22): `docker compose
  config -q` validates. Remaining for the slice: a side-by-side smoke vs the deployed legacy
  backend (same env, diff the wire), rolled into the migration-wide parity gate.
- [x] (2026-07-29) **Real relay verification.** The test deployment was pointed at the real coturn
  `dev-b` relay (`TURN_SECRET` = base64 of its `static-auth-secret`, realm
  `turn-dev-b.novasama-tech.org`, its 3478/5349 ICE URLs). ✅ Demo (2026-07-29): `POST
  /api/v1/turn/issue` on `identity.dotspark.app` returned 201; the password recomputed
  independently from the relay secret matched; a raw STUN/TURN **Allocate** against
  `turn-dev-b:3478` returned `0x0103` success with `XOR-RELAYED-ADDRESS 51.15.72.62:<port>` —
  the relay accepts our credentials end to end.

## Risks / notes

- The issuer and the actual TURN server (coturn) must share `TURN_SECRET` and algorithm — this
  service only mints credentials; nothing here validates against a real relay.
- `servers` echoes deployment config verbatim; the capture pins JS `URL.toString()`
  normalization so the Rust port must not re-normalize differently.
- Stateless: N replicas need no coordination (random ids make username collisions negligible).
