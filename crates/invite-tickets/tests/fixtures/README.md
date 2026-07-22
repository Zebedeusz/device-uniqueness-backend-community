# Golden legacy fixtures — `POST /api/v1/invitation-ticket/claim`

`legacy-wire.json` holds request/response pairs captured from the **real legacy
TypeScript route** on 2026-07-22, against `paritytech/identity-backend` at
commit `c8369d936b41e875f2434dc2931878b7dafe2d1a`. `tests/legacy_wire.rs`
replays them against this service.

## How they were captured

The harness in `capture/invitation-ticket.golden-capture.test.ts` was dropped
into the legacy repo at `apps/identity-backend/src/routes/v1/__tests__/` and
run once with:

```
pnpm --filter identity-backend-container exec vitest run \
  src/routes/v1/__tests__/invitation-ticket.golden-capture.test.ts
```

It mounts, unmodified, the legacy
`makeInvitationTicketRouteWithoutDependencies` (real zod schemas including the
SS58 refine + the real `createOpenAPIHono` problem-details `defaultHook`)
behind the real `verifyJwt` middleware, mirroring the `routes/v1/mod.ts`
mounting, over a stubbed `ClaimInvitationTicketShell` keyed by SS58 address.
It then fires the frozen request matrix and records status, content-type,
`Retry-After`, and body for each pair.

Notable captured facts:

- Empty `who` reports **both** `Too small: expected string to have >=1
  characters` and `Invalid SS58 address format` at `#/who` (zod v4 checks are
  non-aborting).
- A checksum-corrupted but base58-valid address is the same
  `Invalid SS58 address format` 400 — unlike the dim-ticket route, SS58
  validity here is a schema-level 400, never a 500.
- Missing JSON content-type → 500 (Hono skips validation; the handler crashes).
- Wrong method / collection path / unknown path → the Hono plain-text
  `404 Not Found`.

## Approximations (each is deliberate and bounded)

- **Global error hook**: the harness reproduces the two terminal branches of
  `app.ts`'s `app.onError` verbatim (`HTTPException` passthrough + the literal
  `{"error":"Oops! Something went wrong."}` 500) instead of booting the whole
  legacy `makeApp` (DB, OTel, Sentry). The 500 body is a one-line literal in
  `app.ts`; nothing else in the hook shapes this route's responses.
- **Rate limiting (429)**: not mounted, so not captured. The legacy 429 body is
  pinned by reading `middleware/rate-limit.middleware.ts` (`tooManyRequests`):
  problem details `too-many-requests` / `Too Many Requests` /
  `Rate limit exceeded. Please retry after {n} seconds.` + `Retry-After` —
  which `http/error.rs` reproduces byte-for-byte.
- **JWTs**: legacy verified HS256 tokens; this service verifies
  identity-service Ed25519 tokens. The three 401 *bodies* are
  algorithm-independent and captured; authenticated replays mint an EdDSA
  token instead of reusing the captured HS256 one.
- **DB-dependent cases** (`post-200-*`, `post-409-*`, `post-422-*`,
  `post-500-shell-defect`): the stubbed shell fixes their keys, timestamps,
  and signature bytes; the replay compares the frozen response builder and
  error renderers against the captured bodies. Full-path composition against
  live Postgres is `tests/claim_live_pg.rs` (`--ignored`), and the Phase 1
  demo in `docs/plans/active/invite-claim.md`.
