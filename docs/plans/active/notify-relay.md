> Stage: active. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Preserve the shipping `POST /api/v1/notify` behavior in an independently deployable Rust service.
- Deliver the existing encrypted chat payload through FCM, APNs alert, and APNs VoIP without changing either mobile client.
- Isolate push credentials and provider failures from auth, username registration, and username-read.

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
- Gaps: no verify-only sibling helper or notify route exists in the Rust workspace.

## Legacy provider behavior
- What exists: the TypeScript reference is `paritytech/identity-backend`, under `apps/identity-backend/src/routes/v1/notify/` and `apps/identity-backend/src/infrastructure/adapters/notifications/`.
- ✅ Demo: legacy iOS and Android clients receive APNs/VoIP/FCM pushes through `/api/v1/notify`.
- Gaps: push secrets and failures share the legacy monolith's failure domain.

# MVP phases (ship-shaped, demoable)

## Phase 1: Compatible relay boundary
- **Goal**: accept and validate existing mobile requests behind verify-only JWT auth.
- **Scope checklist**:
  - [ ] Add an independently deployable notify crate/binary with liveness and readiness.
  - [ ] Add verify-only Ed25519 JWT validation from configured public-key/JWKS material; no runtime call to identity-service is required after startup.
  - [ ] Accept `deviceToken`, `pushId`, `message`, `voip`, and optional `platform` / `bundlerId` exactly as the clients send them.
  - [ ] Preserve platform detection and request validation from the legacy route.
  - [ ] Preserve the response fields `success`, `platform`, `sent`, `failed`, `messageId`, and `errors`, including legacy provider-failure `200` responses.
  - [ ] Define an internal provider trait and fake FCM/APNs implementations for contract tests.
  - [ ] Keep the service stateless and DB-free.
- **✅ Demo**: replay representative iOS and Android requests with a real identity-service JWT; fake providers receive the expected flat payload, and both mobile response models decode the result.
- **Risks / failure modes**:
  - Android currently expects non-null response fields.
  - Platform auto-detection can misclassify unusual tokens; explicit client hints must win.
  - JWKS/key rotation behavior must fail closed without coupling request handling to identity-service uptime.

## Phase 2: Android FCM delivery
- **Goal**: deliver the existing Android chat push to a real test device.
- **Scope checklist**:
  - [ ] Add FCM credentials only to the notify service's fail-fast configuration.
  - [ ] Send a high-priority data message with the flat `pushType`, `pushId`, and `message` payload expected by Android.
  - [ ] Map FCM success, invalid token, and provider errors to the frozen response contract.
  - [ ] Add provider timeout, bounded retry policy, structured logs, and delivery counters.
- **✅ Demo**: a shipping-compatible Android request returns a successful response, and a test device receives and processes the push.
- **Risks / failure modes**:
  - Provider retries must not duplicate delivery beyond accepted legacy behavior.
  - Credentials must never appear in logs or error bodies.

## Phase 3: iOS APNs and VoIP delivery
- **Goal**: deliver both existing iOS notification modes to a real test device.
- **Scope checklist**:
  - [ ] Add APNs key, key ID, team ID, environment, alert topic, and VoIP topic configuration only to notify.
  - [ ] Preserve alert payload, mutable-content behavior, topic selection, and optional `bundlerId` override.
  - [ ] Preserve VoIP push type, topic suffix, and expiry behavior.
  - [ ] Map APNs success, invalid token, and provider errors to the frozen response contract.
  - [ ] Add provider timeout, bounded retry policy, structured logs, and delivery counters.
- **✅ Demo**: one alert push and one VoIP push sent through the new service are received and handled by the existing iOS app.
- **Risks / failure modes**:
  - Development and production APNs environments and topics are easy to mix.
  - Apple restricts VoIP pushes to call flows; the relay must preserve existing usage exactly.

## Phase 4: Independent deployment readiness
- **Goal**: make notify safe to deploy without changing other service failure domains.
- **Scope checklist**:
  - [ ] Add a separate image/process definition, service-owned secrets, and readiness checks.
  - [ ] Rate-limit by authenticated subject and route without venue-wide raw-IP coupling.
  - [ ] Prove identity-service, username-read, and registration remain healthy when notify is stopped or provider credentials are invalid.
  - [ ] Document the future gateway route contract without implementing the gateway in this slice.
- **✅ Demo**: stop notify and observe only `/api/v1/notify` fail; restart it with valid secrets and deliver a test push without restarting identity-service.
- **Risks / failure modes**:
  - Gateway and cutover are separate plans; direct service testing is sufficient here.

# Contracts (guardrails)
- Keep `POST /api/v1/notify` and the existing JSON field names.
- Require `Authorization: Bearer <JWT>` and verify with public-key material only.
- Keep provider failures parseable by the shipping clients, including legacy HTTP status behavior.
- Keep notify stateless unless a measured requirement proves persistence is necessary.
- Never read identity-service or username-read databases.
- Push secrets and provider outages must not affect auth or registration.

# Key decisions (decide early)
- JWT public-key distribution: pinned configured JWK/public key first, or startup JWKS fetch with a persisted last-known-good key.
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