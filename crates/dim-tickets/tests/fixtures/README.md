# Golden legacy fixtures — `/api/v1/dim-ticket`

`legacy-wire.json` holds request/response pairs captured from the **real legacy
TypeScript route** on 2026-07-22, against `paritytech/identity-backend` at
commit `c8369d936b41e875f2434dc2931878b7dafe2d1a`. `tests/legacy_wire.rs`
replays them against this service.

## How they were captured

The harness in `capture/dim-ticket.golden-capture.test.ts` was dropped into
the legacy repo at `apps/identity-backend/src/routes/v1/__tests__/` and run
once with:

```
pnpm --filter identity-backend-container exec vitest run \
  src/routes/v1/__tests__/dim-ticket.golden-capture.test.ts
```

It mounts, unmodified, the legacy `makeDIMTicketRouteWithoutDependencies`
(real zod schemas + the real `createOpenAPIHono` problem-details
`defaultHook`) behind the real `verifyJwt` middleware, mirroring the
`routes/v1/mod.ts` mounting, over a stubbed `DimTicketShell` keyed by SS58
address. It then fires the frozen request matrix and records status,
content-type, `Retry-After`, and body for each pair.

Notable captured facts (each one corrected a pre-capture approximation in
this crate):

- The 400 messages are **zod v4** (`Invalid input: expected string, received
  undefined`, `Too small: expected string to have >=1 characters`,
  `Invalid option: expected one of "Game"|"ProofOfInk"`), not the zod v3
  texts this crate originally guessed.
- The `dim` field reports the **same option-list message** for every failure
  mode — missing, null, wrong type, or an unknown literal.
- Unlike the invitation-ticket route, `who` has **no SS58 refine**: only
  min-length is schema-checked; an SS58-invalid string reaches the shell,
  whose typed validation error falls through the route's `catchTags` to the
  global hook → **500**, not 400.
- Malformed JSON → the Hono plain-text 400 (`Malformed JSON in request body`,
  un-spaced `text/plain;charset=UTF-8`).
- Missing JSON content-type → 500 (Hono skips validation; the handler
  crashes).
- Every uncaught failure is the global hook's literal
  `{"error":"Oops! Something went wrong."}` — the 500 body this crate
  previously rendered as `Internal Server Error`.
- Wrong method / collection-path GET / unknown path → the Hono plain-text
  `404 Not Found`.
- `updatedAt` on POST 200 bodies (`new Date()`) and on PENDING GET bodies
  (`Clock.currentTimeMillis`) is stamped at response time, so those two
  captures carry capture-time values; the replay masks `updatedAt` and
  asserts its JS-ISO-millis shape instead.

## Approximations (each is deliberate and bounded)

- **Global error hook**: the harness reproduces the two terminal branches of
  `app.ts`'s `app.onError` verbatim (`HTTPException` passthrough + the
  literal `{"error":"Oops! Something went wrong."}` 500) instead of booting
  the whole legacy `makeApp` (DB, OTel, Sentry).
- **Rate limiting (429)**: not mounted, so not captured. The legacy 429 body
  is pinned by reading `middleware/rate-limit.middleware.ts`
  (`tooManyRequests`) — which `http-common::problem` reproduces
  byte-for-byte.
- **JWTs**: legacy verified HS256 tokens; this service verifies
  identity-service Ed25519 tokens. The 401 *bodies* are algorithm-independent
  and captured; authenticated replays mint an EdDSA token instead of reusing
  the captured HS256 one.
- **Shell-dependent cases**: the stubbed shell fixes keys, timestamps, and
  the `onchainData` payload; the SS58-invalid and shell-defect 500s are
  stub-authored `fail`/`die` outcomes, but the *dialect* they exercise (typed
  errors falling through `catchTags` to the global hook) is the real route
  code. The real shell's SS58 validation lives in
  `features/dim/dim-ticket.shell.ts` (`S.decode(InviteeAddress)`), read
  directly.
- **DB/chain-dependent cases** (`post-200-*`, `post-409-*`, `post-422-*`,
  `get-200-*`, `get-404-not-found`): the replay compares the frozen response
  builders and error renderers against the captured bodies. Full-path
  composition against live Postgres + a live chain is the Phase 1 demo in
  `docs/plans/active/dim-tickets.md`.
