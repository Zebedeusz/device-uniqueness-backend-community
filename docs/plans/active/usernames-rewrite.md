> **Superseded in part (2026-08-18).** The V1 wire dialects this plan captured and froze were
> removed: every service now renders one JSON error envelope (`{error}`, plus `fields` on per-field
> validation), and the `legacy_wire.rs` replay suites plus their `legacy-wire.json` fixtures are
> deleted. Phases below that describe capturing, freezing, or replaying those bodies are historical
> record, not current behavior — see `CHANGELOG.md` (Unreleased → Removed).

# Usernames surface rewrite — frozen legacy dialect

Goal: end the invented-dialect debt on the whole `/api/v1/usernames` surface.
Both owning crates predated `http-common` and spoke a made-up
`{error, message}` JSON dialect; the legacy wire behavior was never
golden-captured, and two legacy endpoints were never ported. This slice
captures the real legacy router, rewrites both HTTP shells against the
fixtures, ports `GET /{username}`, and deliberately drops the sunset list
endpoint.

Provenance: fixtures captured 2026-07-23 from `paritytech/identity-backend`
@ `c8369d936b41e875f2434dc2931878b7dafe2d1a`
(`crates/username-indexer/tests/fixtures/`,
`crates/device-attestation/tests/fixtures/`, READMEs there). The capture
settled the load-bearing unknown: legacy's `.use('/', …)` middleware bound
**only the collection root**, so CORS/ETag/JWT/rate-limits applied far more
narrowly than the route docs implied (see the middleware-scope table in the
indexer fixtures README).

## Status

- [x] Phase 0 — golden capture (103 fixtures, two files, archived harness,
  middleware-scope probe notes). 2026-07-23.
- [x] Phase 1 — `http-common`: `invalid_query` / `invalid_param` /
  `invalid_header` renderers + captured zod message helpers. 2026-07-23.
- [x] Phase 2 — `username-indexer`: frozen dialect (query/param problem
  details, `{"error":"Invalid cursor"}`, `{"error":"Username not found"}`,
  429 problem body), legacy validation semantics (limit **clamp**, unknown
  params ignored, multi-error collection), the `v1DigitsLengthBound` filter,
  the integer-digit response render, JS-ISO millis dates, new
  `GET /api/v1/usernames/{username}`, `standard_layers` + `hono_not_found`,
  shared `ConfigError`, OpenAPI tags, fixture replay suite. 2026-07-23.
- [x] Phase 3 — `device-attestation` usernames shell: `AuthSubject` (frozen 401
  trio), subject-keyed rate limit, the three-phase zod validation semantics
  (incl. the captured 500 quirks), captured 409/500 bodies, `?version`
  (v0 default) + v0 response shape, collection-root CORS, service-wide
  `hono_not_found` fallback, fixture replay suite. Old `AuthAccount`
  extractor deleted. 2026-07-23.
- [x] Phase 4 — gateway: `@username_reads` (GET-only) matcher to :8081;
  POST/OPTIONS fall through to device-attestation-api. 2026-07-23.
- [x] Phase 5 — boundary script (indexer holds no JWT material), apidoc-gen
  route list + regenerated artifacts. 2026-07-23.
- [x] Compose stack end-to-end curl pass through the gateway: search 200
  over live Paseo projection data (integer-digit render `alistair.1` +
  JS-ISO-millis dates confirmed on real chain rows), real lookup 200
  (`alistair.01` → `candidateAccountId`), lookup 404 body, dropped-list
  Hono 404, root OPTIONS 204 + captured CORS headers, `/available`
  no-token → 401 trio (the recorded hardening divergence), param/query 400
  dialects, 61st rapid search → 429 + `Retry-After: 60`. 2026-07-23.
  (Authed `/available`/register 200s are covered by the replay suites; the
  live-token path needs the auth handshake and is part of the migration-wide
  parity gate.)

## Deliberate divergences (each frozen by test or fixture evidence)

1. **Deprecated `GET /api/v1/usernames` list endpoint dropped.** Its
   `Sunset` header (2026-06-22) has passed. The gateway routes bare-path GET
   to the indexer, whose fallback answers the Hono `404 Not Found`. Capture
   evidence retained (`list-200-evidence-*`); the replay test pins the 404.
   **`GET /api/v1/usernames/{username}` lookup dropped too (2026-07-23):**
   deprecated in favour of paginated `search`; it now serves the same Hono
   404 via the fallback. Capture evidence retained (`get-*`); the replay
   test pins the 404s.
2. **RESERVED-row visibility.** The indexer serves the chain-derived
   projection: only ASSIGNED rows exist. Reserved-but-unassigned lookups
   404, search omits them, `status` is always `ASSIGNED`, and `onchainData`
   is always `null` (legacy could return block data). Evidence:
   `get-200-reserved-evidence` (lookup fixtures retained although the
   endpoint itself is now dropped — see divergence 1).
3. **`/available` is JWT-gated and subject-rate-limited here; legacy served
   it completely unauthenticated with no rate limit** (proved by the
   capture's auth probes — an accident of the `.use('/', …)` root-only
   middleware scoping). Kept as hardening; the replay test asserts the
   divergent 401 trio. **Cutover risk ruled out by client-code audit
   (2026-07-23):** both shipping apps always authenticate this call —
   Android `UsernameApi.checkUsername` is `@CallWithBearerToken`
   (`feature_usernames_impl/.../UsernameApi.kt`), iOS
   `availableUsernameWrapper` uses `createJWTAuthorizedRequestWrapper`
   (`Common/Username/Networking/UsernameOperationFactory.swift`) — and both
   send `?version=v1` and parse the `{_tag, value}` shape this service
   serves. The desktop app does not call the endpoint. The claim flow mints
   the JWT before any availability check, so no anonymous traffic exists.
   **The unused v0 response shape (the legacy flat record, the captured
   default) was removed 2026-07-23:** the response is always v1, a missing
   `?version` defaults to v1, and `?version=v0` now 400s with
   `Invalid option: expected one of "v1"`. The v0 captures are retained as
   evidence; the replay test pins the divergent responses.
4. **Rate-limit keying.** Public reads: per-IP (legacy `search` class was
   jwt-or-ip). Writes: per-subject, never IP.
5. **Cursors are not interchangeable across cutover.** Same unsigned
   base64url-JSON dialect, different payload fields; a legacy cursor
   presented here yields the captured `400 {"error":"Invalid cursor"}`.
6. **Data source.** Availability/registration read People Chain
   (`UsernameOwnerOf`) + the reservation outbox, not the legacy Postgres
   table; digit-availability values can differ while chain and legacy DB
   disagree.
7. **No response ETags.** Legacy's collection-root `etag({weak:true})`
   stamped weak ETags on POST responses; an entity tag on a POST response
   has no caching semantics, so it is not reproduced. The root's
   `access-control-allow-origin: *` and OPTIONS preflight ARE reproduced.
8. **DeviceCheck lanes deferred** to the attestation slice
   (`PAYMENT_REQUIRED` 200, the non-trio Device-Token 401,
   `device_check_available` advisories). Fixtures parked behind the frozen
   `DEFERRED_DEVICECHECK` list in the device-attestation replay test.
9. **dotns gating is config-driven** (`DOTNS_GATEWAY_ENABLED`, default
   enabled, freshness bounds 600s/600s via env) — legacy read the bounds
   from chain constants. The capture environment had the gateway disabled;
   the disabled 400 replays through a disabled-config router.
10. **Auth-handshake routes are out of scope**: they still speak the older
    `{error, message}` dialect pending their own capture slice. The
    service-wide fallback change means unknown paths on device-attestation-api now
    404 in the Hono text dialect (previously axum's empty 404).
11. **Error bodies are no longer byte-frozen (2026-07-23).** No client
    parses error bodies, so the reproduced legacy quirks were retired on
    the usernames surface while the 401/404/409 status codes stay: the
    malformed-JSON Hono text 400 is a normal JSON 400 (same message); the
    intentional 500s are normal 400s — a regex-failed `candidateSignature`
    is the plain pattern field error (legacy fed the raw string to the
    wasm verifier, which threw), and the body parses as JSON regardless of
    content type (legacy: available crashed to an opaque 500, register
    silently validated an empty object). Validation messages and the
    problem-details 400 shape are unchanged; the affected captures are
    retained as evidence and the replay suite pins the divergent
    responses.

## Verification

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  (includes both `legacy_wire` replay suites: 48 + 55 fixtures classified
  exactly once, frozen counts).
- `bash scripts/verify_compose_boundaries.sh`.
- Live Postgres semantics: `just test-live` (device-attestation outbox allocation across
  every status + indexer `pagination_live`).
- Compose stack: `docker compose up -d` then through the gateway (`:8000`):
  search 200 envelope; `GET /api/v1/usernames/nosuchname.11` →
  `404 Not Found` text (lookup dropped); `GET /api/v1/usernames` →
  `404 Not Found` text;
  `OPTIONS /api/v1/usernames` → 204 + CORS; `/available` without token →
  401 trio; with token → v1 200s (v0 removed); register validation failure →
  problem+json; rapid-fire search → 429 + `Retry-After: 60`.
