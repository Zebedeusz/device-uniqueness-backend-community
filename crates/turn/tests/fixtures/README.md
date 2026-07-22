# Golden legacy fixtures — `POST /api/v1/turn/issue`

`legacy-wire.json` holds request/response pairs captured from the **real legacy
TypeScript route** on 2026-07-22, against `paritytech/identity-backend` at
commit `c8369d936b41e875f2434dc2931878b7dafe2d1a`. `tests/legacy_wire.rs`
replays them against this service.

## How they were captured

The harness in `capture/turn.golden-capture.test.ts` was dropped into the
legacy repo at `apps/identity-backend/src/routes/v1/__tests__/` and run once
with:

```
pnpm --filter identity-backend-container exec vitest run \
  src/routes/v1/__tests__/turn.golden-capture.test.ts
```

It mounts, unmodified, the legacy `makeTurnIssueRoute` (real zod schemas +
the real `createOpenAPIHono` problem-details `defaultHook`) behind the real
`verifyJwt` middleware, mirroring the `routes/v1/mod.ts` mounting, over the
**real** `IssueTurnCredentialsUseCase` with a pinned `TurnServiceConfig`
(secret = 32 bytes of `0x42`, TTL 30 min, realm `example.org`;
`ICE_SERVERS=stun:stun.example.com:3478,turn:turn.example.com:3478?transport=udp`).
Two apps were built — SHA1 (the deployment default) and SHA256 — to pin the
algorithm dimension.

Notable captured facts:

- **Missing JSON content-type → 201**, and **no body at all → 201**: Hono
  skips validation and the legacy handler optional-chains the absent body
  (`body?.regionHint`) — unlike the dim/invite handlers, where the same skip
  crashed into a 500. The quirk is frozen.
- An empty body **with** a JSON content-type is the Hono plain-text
  `Malformed JSON in request body` 400 (as is `{not json`).
- The success status is **201**, not 200.
- `regionHint` accepts string/null/absent and is ignored; any other type is a
  zod v4 400 at `#/regionHint`; non-object bodies (array/string/`null`
  literal) are the `#/` object error.
- `servers` echoes the configured URL list verbatim — JS `URL.toString()`
  did not re-normalize either the `stun:` or the `turn:…?transport=udp` form.
- `username = "{unixExpiry}:{hex(8 bytes)}"`, `password =
  base64(HMAC(secret, username))` — verified in the replay by **recomputing
  the HMAC over each captured username with the pinned secret** (both SHA1
  and SHA256 captures verify), and the same check runs against this service's
  live responses. `ttl` is the configured TTL in whole seconds (1800).

## Approximations (each is deliberate and bounded)

- **Global error hook**: the harness reproduces the two terminal branches of
  `app.ts`'s `app.onError` verbatim instead of booting the whole legacy
  `makeApp`. No captured case reaches it — this route has no failure paths of
  its own.
- **Rate limiting (429)**: not mounted, so not captured. The legacy 429 body
  is pinned by reading `middleware/rate-limit.middleware.ts`
  (`tooManyRequests`) — which `http-common::problem` reproduces
  byte-for-byte.
- **JWTs**: legacy verified HS256 tokens; this service verifies
  identity-service Ed25519 tokens. The 401 *bodies* are algorithm-independent
  and captured; authenticated replays mint an EdDSA token instead of reusing
  the captured HS256 one.
- **Credential dynamism**: the 201 bodies embed capture-time expiries and
  random ids, so the replay compares `servers`/`ttl` exactly and pins
  username shape + the HMAC construction cryptographically rather than
  comparing credential bytes.
