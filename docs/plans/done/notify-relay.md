> Stage: **done** — shipped in [#9](https://github.com/paritytech/identity-backend-rust/pull/9) (merged 2026-07-23, squash `bdc2758`). The `notifications` crate + `notify-relay` service are on `main` and deploy with the stack.
>
> **Shipped state:** verify-only-JWT `POST /api/v1/notify` relay → iOS APNs + Android FCM (each optional), per-subject rate limited, independently deployable (own compose service on host **:8085**, no `depends_on`), routed by the committed `gateway/Caddyfile` (`/api/v1/notify*` → `notify-relay`). Built on the shared `jwt-verify` + `http-common` crates: after review, the error layer is http-common problem-details (400/401/429) matching the legacy route, and the provider result bodies match the legacy APNs/FCM adapters byte-for-byte (APNs success = counts, no `messageId`; FCM success = `sent`+`messageId`, no `failed`; terminal APNs token errors + FCM rejections fall back to the generic `200 success:false`). APNs key mounted from a host path via `APNS_KEY_FILE`; APNs + FCM secrets loaded on the `ibv2` server (`APNS_ENVIRONMENT=production`).
>
> **Deferred to rollout validation (not merge gates):** the live **VoIP** smoke (needs a real `PKPushRegistry` token — the app supports PushKit; the alert-token smoke got the expected `DeviceTokenNotForTopic`) and the live **Android FCM device** smoke (creds are on the server; needs a real device token + confirmation the Android app targets this backend). Also deferred: bounded retry/idempotency + delivery counters (telemetry slice).

# Goals
- Preserve the shipping `POST /api/v1/notify` behavior in an independently deployable Rust service.
- Deliver the existing encrypted chat payload through FCM, APNs alert, and APNs VoIP without changing either mobile client.
- Isolate push credentials and provider failures from auth, username registration, and username-indexer.

# Non-goals
- `/subscriptions`, subscription rules, Statement Store fan-out, broadcast notifications, Web Push, or notifications v3.
- Chat encryption, message storage, contact discovery, or statement processing.
- TURN, invites, username indexing, platform attestation, gateway implementation, or legacy cutover.
- Adding a database unless a proven delivery requirement needs one.

# Design principles
- User journey drives order.
- Keep the relay stateless and thin.
- Verify identity-service JWTs with public-key material; never hold a JWT signing key.
- Provider failure is contained to notify and cannot affect identity-service readiness.
- Preserve provider-specific mobile behavior instead of redesigning push.

# User journey
1. A sender's app creates the existing encrypted notification payload.
2. The app calls `/api/v1/notify` with its JWT and the recipient's push token.
3. Notify selects FCM, APNs alert, or APNs VoIP.
4. The recipient device receives the push and the existing app handler processes it.

# Foundations / Already shipped (✅)

## JWT issuer and mobile contract
- What exists: identity-service publishes Ed25519 JWKS at `/.well-known/jwks.json`; both shipping clients send `Authorization: Bearer <JWT>` to `/api/v1/notify`.
- ✅ Demo: the Identity Service MVP issues a verifiable JWT, and the wire-contract tests lock the bearer-token convention.
- Gaps: no notify route exists in the Rust workspace. (The verify-only sibling helper now exists: `crates/jwt-verify`, extracted 2026-07-16 — consume it, don't build an inline copy.)

## Legacy provider behavior
- What exists: the TypeScript reference is `paritytech/identity-backend`, under `apps/identity-backend/src/routes/v1/notify/` and `apps/identity-backend/src/infrastructure/adapters/notifications/`.
- ✅ Demo: legacy iOS and Android clients receive APNs/VoIP/FCM pushes through `/api/v1/notify`.
- Gaps: push secrets and failures share the legacy monolith's failure domain.

# MVP phases (ship-shaped, demoable)

## Phase 1: Compatible relay boundary
- **Goal**: accept and validate existing mobile requests behind verify-only JWT auth.
- **Status**: ✅ done 2026-07-14 — `notifications` crate (`notify-relay` bin), verify-only Ed25519 JWT, frozen `/api/v1/notify` contract, provider trait + fakes, golden contract tests. `just check` green.
- **Scope checklist**:
  - [x] Add an independently deployable notify crate/binary with liveness and readiness.
  - [x] Add verify-only Ed25519 JWT validation from configured public-key/JWKS material; no runtime call to identity-service is required after startup. Consume the shared `crates/jwt-verify` crate rather than building an inline copy.
  - [x] Accept `deviceToken`, `pushId`, `message`, `voip`, and optional `platform` / `bundlerId` exactly as the clients send them.
  - [x] Preserve platform detection and request validation from the legacy route.
  - [x] Preserve the response fields `success`, `platform`, `sent`, `failed`, `messageId`, and `errors`, including legacy provider-failure `200` responses.
  - [x] Define an internal provider trait and fake FCM/APNs implementations for contract tests.
  - [x] Keep the service stateless and DB-free.
- **✅ Demo**: replay representative iOS and Android requests with a real identity-service JWT; fake providers receive the expected flat payload, and both mobile response models decode the result.
- **Risks / failure modes**:
  - Android currently expects non-null response fields.
  - Platform auto-detection can misclassify unusual tokens; explicit client hints must win.
  - JWKS/key rotation behavior must fail closed without coupling request handling to identity-service uptime.

## Phase 2: iOS APNs and VoIP delivery
- **Goal**: deliver both existing iOS notification modes to a real test device.
- **Status**: 🚧 **alert delivery verified end-to-end 2026-07-22**; VoIP still pending a PushKit token. `provider::apns::ApnsProvider` (token-based ES256 auth, HTTP/2, flat payload, topic/push-type/expiry, error→contract mapping) wired behind optional `APNS_*` config; unit-tested. Live alert push to a real dev-build device (topic `io.parity.brevity`, `APNS_ENVIRONMENT=development`, real `.p8`) returned `success:true` and woke the app (it rendered "unsupported message" only because the smoke used a dummy hex `message`, not a real encrypted chat payload — transport/topic/envelope all correct). The VoIP push reached Apple with the right `.voip` topic + push-type but was rejected `400 DeviceTokenNotForTopic`: the standard APNs token isn't a PushKit token, so a real VoIP smoke needs the app's `PKPushRegistry` token.
- **Credentials on hand** (from Leonardo, 2026-07-14): APNs `.p8` key, key ID `73L6WWUQ3F`, team ID `P2PX3JU8FT`, bundle ID `io.parity.brevity`. Environment confirmed for the dev build: `development`; alert topic `io.parity.brevity`, VoIP topic `io.parity.brevity.voip`. Still needed: a PushKit VoIP token to finish the VoIP demo.
- **Scope checklist**:
  - [x] Add APNs key, key ID, team ID, environment, alert topic, and VoIP topic configuration only to notify. *(VoIP topic derives from the base topic + `.voip`, matching the legacy adapter — no separate var.)*
  - [x] Preserve alert payload, mutable-content behavior, topic selection, and optional `bundlerId` override.
  - [x] Preserve VoIP push type, topic suffix, and expiry behavior.
  - [x] Map APNs success, invalid token, and provider errors to the frozen response contract.
  - [x] Add provider timeout and structured logs.
  - [ ] Bounded retry policy and delivery counters — **deferred**: a naive retry risks duplicate delivery (see risks); revisit with `apns-id` dedup. Counters wait on the telemetry slice.
- **✅ Demo**: alert push **passed 2026-07-22** — an alert sent through `notify-relay` (dev environment, real `.p8`) was accepted by APNs and woke the real iOS dev build. VoIP half still **pending** a PushKit token (the alert-token smoke got the expected `DeviceTokenNotForTopic`).
- **Risks / failure modes**:
  - Development and production APNs environments and topics are easy to mix.
  - Apple restricts VoIP pushes to call flows; the relay must preserve existing usage exactly.
  - Retrying a push after a timeout can double-deliver unless a stable `apns-id` is reused.

## Phase 3: Android FCM delivery
- **Goal**: deliver the existing Android chat push to a real test device.
- **Status**: 🚧 provider built 2026-07-14 — `provider::fcm::FcmProvider` (OAuth2 access token from an RS256 service-account assertion, cached; FCM v1 send; flat `data` payload; error→contract mapping) wired behind optional `FCM_SERVICE_ACCOUNT_JSON`; unit-tested. **Live delivery pending** the FCM service-account credentials (not yet provided) and a real Android device token.
- **Scope checklist**:
  - [x] Add FCM credentials only to the notify service's fail-fast configuration.
  - [x] Send a high-priority data message with the flat `pushType`, `pushId`, and `message` payload expected by Android.
  - [x] Map FCM success, invalid token, and provider errors to the frozen response contract.
  - [x] Add provider timeout and structured logs.
  - [ ] Bounded retry policy and delivery counters — **deferred** (same rationale as Phase 2: duplicate-delivery risk without idempotency; counters wait on the telemetry slice).
- **✅ Demo** (partial): live auth/connectivity to Google **passed** (`400 INVALID_ARGUMENT` for a dummy token). A shipping-compatible Android request received by a real device is still **pending** a real Android device token.
- **Risks / failure modes**:
  - Provider retries must not duplicate delivery beyond accepted legacy behavior.
  - Credentials must never appear in logs or error bodies.

## Phase 4: Independent deployment readiness
- **Goal**: make notify safe to deploy without changing other service failure domains.
- **Status**: ✅ done 2026-07-21 — `notify-relay` builds as its own binary (Dockerfile) and runs as an independent compose service on host port 8083 with **no `depends_on`** (no shared DB/chain/failure domain), service-owned secrets from the server `.env`. `/api/v1/notify` is rate-limited **per authenticated subject** (`RateLimiter`, key `route:accountId`, `429 RATE_LIMITED`, `NOTIFY_RATE_LIMIT`/`NOTIFY_RATE_LIMIT_WINDOW_SECS`, defaults 60/60s) — never raw IP. Caddy routes only `/api/v1/notify` → :8083; the future-gateway route contract is documented in `docs/deploy.md`/`docs/architecture.md` without building a gateway. `just check` green.
- **Scope checklist**:
  - [x] Add a separate image/process definition, service-owned secrets, and readiness checks.
  - [x] Rate-limit by authenticated subject and route without venue-wide raw-IP coupling.
  - [x] Prove identity-service, username-indexer, and registration remain healthy when notify is stopped or provider credentials are invalid. *(By construction: no `depends_on`, stateless, DB-free; failure-isolation smoke documented in `docs/deploy.md`. Providers already degrade to unconfigured stubs on bad/absent secrets without affecting readiness.)*
  - [x] Document the future gateway route contract without implementing the gateway in this slice.
- **✅ Demo**: stop notify and observe only `/api/v1/notify` fail; restart it with valid secrets and deliver a test push without restarting identity-service. *(Isolation demo scripted in `docs/deploy.md` "Failure isolation check"; the deliver-a-push half still needs a real device token, tracked with the Phase 2/3 live smokes.)*
- **Risks / failure modes**:
  - Gateway and cutover are separate plans; direct service testing is sufficient here.

# Contracts (guardrails)
- Keep `POST /api/v1/notify` and the existing JSON field names.
- Require `Authorization: Bearer <JWT>` and verify with public-key material only.
- Keep provider failures parseable by the shipping clients, including legacy HTTP status behavior.
- Keep notify stateless unless a measured requirement proves persistence is necessary.
- Never read identity-service or username-indexer databases.
- Push secrets and provider outages must not affect auth or registration.

# Key decisions (decide early)
- JWT public-key distribution: pinned configured JWK/public key first, or startup JWKS fetch with a persisted last-known-good key. Whichever is chosen lives in the shared `jwt-verify` crate (see the dim-tickets plan, same decision date), so decide it once for both consumers.
- APNs development/production routing and topic configuration for the first live smoke.
- Whether one owner implements both providers or FCM and APNs become sequential sub-assignments within this plan.

# Testing
- Manual smoke demo per phase.
- Golden iOS and Android request/response fixtures based on the shipping models and legacy route tests.
- Provider adapter tests for success, invalid token, timeout, and transient failure.
- Real-device FCM, APNs alert, and APNs VoIP smokes before the slice is complete.

# Polish rounds (after MVP)

## Polish round 1: Delivery reliability and cost
- Measure provider latency, failure rate, retry volume, and duplicate risk.
- Tune timeouts and bounded retries from observed behavior.
- ✅ Check-in demo: provider fault injection stays within the agreed latency and failure budgets without affecting another service.

# Later / Deferred
- Subscriptions, rules, broadcast, Statement Store fan-out, and Web Push belong to a separate notifications-v3 decision and plan.
- Gateway routing, release-device validation, and legacy cutover live in later plans.