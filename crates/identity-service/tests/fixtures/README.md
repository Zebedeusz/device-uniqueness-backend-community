# Golden legacy fixtures — `/api/v1/usernames` (POST surface)

`legacy-wire.json` holds request/response pairs for `POST /api/v1/usernames`
(register) and `POST /api/v1/usernames/available`, captured from the **real
legacy TypeScript usernames router** on 2026-07-23, against
`paritytech/identity-backend` at commit
`c8369d936b41e875f2434dc2931878b7dafe2d1a`. `tests/legacy_wire.rs` replays
them against this service.

The GET surface from the same capture run, the archived harness, and the
middleware-scope probe notes live in
`crates/username-indexer/tests/fixtures/` (see `capture/` and
`capture-notes.json` there). Read that README's middleware-scope table
first — it is the load-bearing capture fact.

## Mounts

- **Mount A** (most fixtures): the real `makeUsernamesRoute` mounted exactly
  as `routes/v1/mod.ts` does, over PGlite + real migrations. Register crypto
  is REAL sr25519 (fixed dev mnemonic `DEV_PHRASE`, path
  `//usernames-capture`): the 202s and the `Invalid signature.` 400 are
  genuine verifications, and the recorded request bodies replay verbatim
  against any correct verifier.
- **Mount B** (`register-*-devicecheck-*`, `register-*-dotns-*`): the
  register route driven directly (requests fired at the route root `/`)
  with a stubbed `RegisterUsernamesV1RouteConfig`
  (`verifySignature → true`, `dotnsGatewayEnabled: true`, freshness bounds
  600/600) and a middleware forcing each DeviceCheck decision — the only way
  to reach those lanes. Response-altitude fixtures only. The TestClock was
  advanced to t=2000s; dotns `signedAt` values are relative to that.

## Notable captured facts

- **`POST /available` is completely ungated in legacy** — no JWT, no rate
  limit (the three `available-auth-probe-*` fixtures return 200). This crate
  keeps its JWT gate as a recorded hardening divergence — flagged in the
  plan doc; flip to parity before cutover if shipping clients call it
  anonymously.
- Register IS gated: the 401 trio problem+json bodies are captured
  (`http-common`'s frozen renderers).
- Register body validation → `invalid-body-property-value` problem+json,
  all failures in zod field order (`candidateAccountId, username,
  candidateSignature, ringVrfKey, proofOfOwnership,
  consumerRegistrationSignature, identifierKey, preferredDigits, dotns`).
  Custom messages: `Digits must be between 01-99`,
  `Invalid ss58 address.`, `Invalid signature.`,
  `Username sum exceeds the maximum length: (29).`,
  `dotNS gateway is not enabled in this environment.`, dotns
  future/stale messages, and raw zod texts for the rest (`Invalid string:
  must match pattern /^(0x)?[a-fA-F0-9]+$/`, …).
- **Non-decodable hex in a `.transform(decodeHex)` field → 500**
  (`{"error":"Oops! Something went wrong."}`): zod v4 runs the transform
  even after the regex issue, and `decodeHex('nothex')` throws
  (`register-400-signature-not-hex`, `register-400-signature-wrong-length`
  — note their names say 400 but the captured status is **500**). A
  wrong-length-but-valid-hex `identifierKey` is a normal 400. **Quirk
  retired 2026-07-23**: both cases are normal pattern 400s now; the
  captures are evidence.
- Bad-base64 `Device-Token-iOS` header → the **header dialect**:
  `invalid-request-header-format` / `"Invalid Header Value"` /
  "The request headers contain an invalid value.", error
  `Invalid base64-encoded string` at `#/Device-Token-iOS`.
- Missing content-type: register → **400** with every field reported
  missing (Hono parses the body as empty); available → **500** (handler
  crash, dim-style). **Both quirks retired 2026-07-23**: the body parses
  as JSON regardless of content type; the captures are evidence.
- Malformed JSON → Hono plain-text 400 (`Malformed JSON in request body`).
  **Retired 2026-07-23**: same message, normal JSON 400 body now.
- 409 bodies: `Preferred digits 88 already taken for username preftkn`,
  `No digits available for username crammed.` — plain `{"error": …}` JSON.
- 202 bodies are snake_case: `{base_username, digits, username,
  device_check_available?}`. Digit selection was made deterministic by
  seeding (98 taken → forced `55`) or `preferredDigits`.
- `available` 200s: legacy v0 (the captured default) is a flat
  `name → INVALID|AVAILABLE|EXHAUSTED` record; `?version=v1` wraps as
  `{_tag:"v1", value:{name:{status, availableDigits?}}}` with
  `availableDigits` ascending 1..99 (never 0), RESERVED rows counting as
  allocated. **The v0 shape was removed from this service on 2026-07-23**
  (unused — both shipping apps send `?version=v1`); the v0 captures are
  evidence only. Object key order is partition/hash-driven — compare as
  JSON values, never bytes. A 30-char lowercase base is AVAILABLE
  (availability has no max-length rule; the ≤29 cap is register-only).
- `OPTIONS /` preflight → 204 with `access-control-allow-origin: *` (hono
  `cors()` defaults) — collection root only.

## Transcribed (unreachable in the harness; dim-429 precedent)

- **429**: rate-limit middlewares were pass-through probes. Body pinned from
  `middleware/rate-limit.middleware.ts` = `http_common::problem::rate_limited`.
- **`UsernameAlreadyTakenError` 409** (insert race): unreachable because any
  seeded 2-digit row is visible to the availability check that runs first.
  Text transcribed from `register/routes.ts`:
  `Username {username}.{digits} already taken. Please try different digits.`
- **Persistence 500**: `{"error":"Failed to persist username registration"}`
  (`register/routes.ts` catchTag).
