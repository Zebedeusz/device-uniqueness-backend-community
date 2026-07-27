# Golden legacy fixtures — `/api/v1/usernames` (GET surface)

`legacy-wire.json` holds request/response pairs captured from the **real
legacy TypeScript usernames router** on 2026-07-23, against
`paritytech/identity-backend` at commit
`c8369d936b41e875f2434dc2931878b7dafe2d1a`. `tests/legacy_wire.rs` replays
them against this service. The POST surface (`register`, `available`) from
the same capture run lives in
`crates/identity-service/tests/fixtures/legacy-wire.json`.

## How they were captured

The harness archived at `capture/usernames.golden-capture.test.ts` was
dropped into the legacy repo at
`apps/identity-backend/src/routes/v1/__tests__/` and run once with:

```
pnpm --filter identity-backend-container exec vitest run \
  src/routes/v1/__tests__/usernames.golden-capture.test.ts
```

Mount A mounts, unmodified, the real `makeUsernamesRoute` (real
`cors()`/`etag()`/`except()` middleware wiring, real `verifyJwt`, real zod
schemas + `createOpenAPIHono` problem-details `defaultHook`, real sr25519
register crypto from a fixed dev mnemonic) exactly as `routes/v1/mod.ts`
mounts it, over `DBTest` (in-memory PGlite with the real migrations, seeded
with fixed rows/dates). Rate-limit middlewares are pass-through probes that
record which paths invoke them (`capture-notes.json`). Mount B (POST-side
only) is described in the identity-service README.

## The middleware-scope discovery (the load-bearing capture fact)

`username/mod.ts` registers cors/etag/JWT/rate-limits with `.use('/', …)`,
which Hono scopes to the **collection root path only**. The probes prove, per
endpoint:

| Endpoint | JWT | Rate limit | CORS | ETag |
|---|---|---|---|---|
| `GET /search` | none (public) | `search` (explicit `.use('/search', …)`) | none | none |
| `GET /{username}` | none (public) | **none** | none | none |
| `GET /` (deprecated list) | none | `search` | yes | weak |
| `OPTIONS /` (root only) | — | — | 204 preflight | — |
| `POST /` (register) | `verifyJwt` | `authActions` | yes | — |
| `POST /available` | **none** | **none** | none | none |

So the Rust GET surface needs **no CORS and no ETag** (both existed only on
the collection root, which serves the dropped list endpoint).
`GET /{username}` had no rate limit in legacy; the endpoint was later
dropped from this crate entirely (removed 2026-07-23, deprecated in favour
of paginated `search`), so its captures are retained as evidence only.

## Notable captured facts

- **Search renders digit suffixes as integers**: a stored `digits = '01'`
  row comes back as `"username": "alicea.1"` (the DTO decodes digits via
  `NumberFromString` before the `${username}.${digits}` render), while
  `GET /{username}` returns `charlie.11` (string render). Frozen legacy
  quirk; the ordering key still uses the *string* digits.
- Query-param validation failures → `invalid-request-parameter-value` /
  `"Invalid Query Parameter Value"` problem+json with `#/field` pointers,
  all failures collected in zod field order.
- Path-param failures (`GET /{username}`) → same type URL,
  `"Invalid Parameter Value"` / "The request path contains an invalid
  parameter value.", message
  `Invalid string: must match pattern /^([a-z]{6,})\.(0[1-9]|[1-9][0-9])$/`.
- Invalid cursor (bad base64url or wrong JSON shape) → plain
  `application/json` `{"error":"Invalid cursor"}`, not problem+json.
- `GET /{username}` miss → `{"error":"Username not found"}` 404; the body
  uses **`candidateAccountId`** while search uses `accountId`.
- `limit=5000` → **200 clamped to 1000** (not a 400); `limit=0/-1/abc/1.5`
  → 400 with zod coerce messages (`Invalid input: expected number, received
  NaN`, `Too small: expected number to be >0`).
- Unknown query params (e.g. `status=RESERVED`) are **ignored** (200).
- `includeOnchainData=false` behaves as **true** (`z.coerce.boolean`
  coerces any non-empty string) — quirk pinned by
  `search-200-include-onchain-false-string`.
- Uppercase prefixes are matched case-insensitively (store lowercases).
- Dotted prefixes (`alicea.`) restrict to lite usernames
  (`full_username IS NULL`).
- Wrong method / unknown subpath / trailing-slash collection path → Hono
  plain-text `404 Not Found`.
- The three `list-200-evidence-*` fixtures document the deprecated
  `GET /` list endpoint (`Deprecation: true`, `Sunset: Mon, 22 Jun 2026`,
  `Link` alternate header). That endpoint is **deliberately not ported**
  (Sunset passed); the fixtures are evidence, never replayed for parity.
- All `get-*` fixtures document the `GET /{username}` lookup endpoint,
  **removed from this crate on 2026-07-23** (deprecated in favour of
  paginated `search`). Like the list captures they are evidence of the
  retired wire contract; the replay suite pins the Hono 404 they now serve.

## Approximations (deliberate and bounded)

- **Global error hook**: the harness reproduces the two terminal branches of
  `app.ts`'s `onError` verbatim instead of booting the whole legacy app.
- **Rate limiting (429)**: pass-through probes, so no 429 was captured. The
  legacy 429 body is pinned from `middleware/rate-limit.middleware.ts` —
  `http-common::problem::rate_limited` reproduces it byte-for-byte.
- **Cursors**: captured `nextCursor` values are unsigned base64url JSON
  (`{key, username, digits, timestamp}`). The harness ran under
  `@effect/vitest`'s TestClock, so captured cursor `timestamp`s are epoch —
  deterministic, and irrelevant to matching (the field is decode-validated
  only). Rust cursors are the same JSON dialect but are not guaranteed
  byte-identical; replays assert decode/round-trip, not byte equality.
- **Data source**: fixtures were seeded into the legacy
  `individuality_usernames` table. This crate serves the chain-derived
  `assigned_usernames` projection, which holds only ASSIGNED rows — the
  RESERVED/FAILED-row fixtures (`search-200-mixed-order` rows 3–4,
  `get-200-reserved-evidence`) document the visibility divergence recorded
  in the plan doc rather than replayable parity.
