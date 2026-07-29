# Device attestation — ship-first plan

> Working agreement: keep phases small; each ends in a runnable demo. Check boxes `[ ]`→`[x]` with
> dates. The plan file is the single source of truth for this slice.

## What ships

The **Genuineness + iOS Uniqueness** scope, implemented inside `identity-service` per
`docs/architecture.md`
("attestation → inside identity-service, NOT a service, NOT a shared lib"):

- **Apple App Attest** (iOS genuineness) — real verification behind the existing
  `POST /api/v1/auth/app-attest/attestations` + `/api/v1/auth/token` scaffold.
- **Google Play Integrity** (Android genuineness) — verify the token the shipping app already
  sends in `/auth/token` headers (`Auth-Attestation-Type: play-integrity`, `Auth-Payload`).
- **Android hardware-keystore (TEE) attestation** (vanilla-APK / no-GMS genuineness) — verify
  the `attestationChain` the shipping vanilla flavor already sends **inline in the `/auth/token`
  JSON body** (`Auth-Attestation-Type: key-attestation`). No new endpoint — the proposed
  two-step `POST /auth/android/attestation` was never built by any client or by legacy (see
  Later).
- **Apple DeviceCheck** (iOS uniqueness / PoUD) — the two-bit device query that resolves the
  free-registration outcome at username claim.
- The `AUTH_ENABLED` / `ENFORCE_AUTH` posture flip on the live deployment (no-op → soft → hard).

# Goals

- A JWT is only issued to a genuine, untampered app instance on a real device when attestation is
  enforced; `appFromOfficialStore` is derived from real attestation evidence, not hardcoded.
- iOS covers **all** iOS requirements: App Attest (genuineness) + DeviceCheck
  (uniqueness).
- Android covers both distribution paths: Play Store (Play Integrity) and vanilla/no-GMS
  (hardware Key Attestation) — GrapheneOS-class devices are first-class via the keystore path.
- Same binary serves every posture: `AUTH_ENABLED=false` (no-op, testnet), soft
  (`ENFORCE_AUTH=false`, log verdicts), hard (reject with the legacy `403 INTEGRITY_FAILED`
  contract).

# Non-goals

- **Android uniqueness/anti-Sybil** — the spec is explicit: nothing exists and nothing was
  implemented in V1. The `{androidId, widevineId}` PoUD signals from the spec are deferred (see
  Later).
- **Queue / payment / registration-voucher logic** — DeviceCheck only *feeds* the INSTANT/QUEUED/
  PAYMENT_REQUIRED decision; the eligibility/queue engine (incl. the QR `lifetimePoUDVoucher`
  from the spec) is its own plan (remaining scope #2 in `docs/architecture.md`).
- **The legacy auth voucher is dropped** (decided 2026-07-24). Legacy `/auth/token` also accepts
  `Auth-Attestation-Type: voucher` + `Auth-Voucher-Secret` — a single-use, DB-backed bypass of
  platform attestation, added as a Web3 Summit workaround. Verified dead surface: **no client
  code exists** (iOS/Android have none) and the only minting endpoint is a debug route disabled
  in production (manual DB inserts are the sole prod path). V2 does not rebuild it; the type is
  rejected like any unknown `Auth-Attestation-Type`. Courtesy: flag the drop to the legacy
  backend owner before cutover in case anyone still hand-mints them.
- **Proof-of-compute gate** — separate concern (design-doc Future Work).
- **Attester-quorum decentralization** — the M-of-N attester direction stays a design thread, not
  this slice.

# Foundations / Already shipped (✅)

- **Auth handshake scaffold** (`crates/identity-service/src/auth/`): challenge mint/consume
  (single-use + TTL, `challenge.rs`), sr25519 account proof over the raw body (`proof.rs`,
  always verified), JWT + refresh issuance (`token.rs`, `refresh.rs`), platform detection from
  `Auth-iOS-Package` / `Auth-Android-Package` headers (`auth.rs::detect_platform`), and the
  shape-preserving App Attest no-op (`app_attest.rs`, `202 {}`).
- **Posture config** (`config.rs`): `auth_enabled` / `enforce_auth` + `attestation_mode()` already
  exist and are logged at the Slice-6 hole in `token.rs`.
- **Rate limiting** on the whole auth group; migrations `0001_auth.sql` (challenges, refresh
  tokens) auto-run on boot.
- **Legacy reference implementation**: the legacy TypeScript backend's auth modules
  ({`app-attest/`, `play-integrity/`, `device-check/`, `auth/`}) — dispatch workflow, validation
  workflows, config names (e.g. `DEVICE_CHECK_KEY_ID`, `DEVICE_CHECK_PRIVATE_KEY`), and tests.
  Port semantics, not the Effect-TS framework.
- ✅ Demo: `just check` green today; the local E2E runbook proves the
  challenge→proof→JWT→register path against the test deployment.

# MVP phases (ship-shaped, demoable)

- [ ] **Phase 1 — iOS App Attest (genuineness).** Fill the Slice-6 hole for iOS.
      *(2026-07-24: code-complete + synthetic-fixture tested; ✅ demo still needs real-device
      captures.)*
  - [x] (2026-07-24) `attestation` dispatch: iOS verdict path in `token.rs`
        (`ios_attestation_verdict`) — package allow-list, required headers, posture gating.
  - [x] (2026-07-24) Real `POST /auth/app-attest/attestations` (`auth/app_attest.rs` +
        `app_attest/verify.rs`): CBOR decode, chain to the Apple root (P-384/P-256, validity
        window), nonce extension check, keyId = SHA256(pubkey) = credentialId, rpIdHash appId
        check, counter 0, dev/prod AAGUID; legacy `401 {_tag, error}` failure bodies; no-op
        `202 {}` preserved while `AUTH_ENABLED=false`.
  - [x] (2026-07-24) Migration `0004_app_attest_keys.sql` (key_id → public_key, receipt,
        sign_count, observability client id; numbered around the concurrent 0003).
  - [x] (2026-07-24) `/auth/token` iOS path: key lookup by `Auth-iOS-KeyId`, assertion verify
        over the shared `client_data_hash` (same bytes as the sr25519 proof), sign-count
        monotonicity + commit, soft (log) / hard (reject) postures.
  - [x] (2026-07-24) Config: `APPLE_APP_ATTEST_APP_IDS` + `IOS_PACKAGE_NAMES` (legacy names,
        comma-separated, fail-fast non-empty when `AUTH_ENABLED=true`); `.env.example` + API
        reference regenerated.
  - **✅ Demo**: captured real-device fixtures (attestation + assertion) verify green through the
    router; a tampered assertion / wrong bundle id / replayed challenge is logged in soft mode and
    rejected in hard mode; `AUTH_ENABLED=false` behaves exactly as today
    (no-op regression fixtures). *(Synthetic-chain unit tests cover every reject reason today —
    28 green; the real-device capture is the remaining gate.)*
  - Risks: Apple's attestation format is CBOR/X.509-heavy — budget for fixture capture from a real
    device (needs the dev iOS app); dev vs prod aaguid mismatch is a classic footgun.

- [ ] **Phase 2 — Android Play Integrity (genuineness, shipping path).**
      *(2026-07-25: code-complete + synthetic-fixture tested — full local pipeline behind
      `PLAY_INTEGRITY_{DECRYPTION,VERIFICATION}_KEY`, plus a **temporary Google-API fallback**
      behind the legacy `GOOGLE_CREDENTIALS`, so real tokens can verify before the key
      exchange lands.)*
  - [x] (2026-07-25) Verify the integrity token from `Auth-Payload` at `/auth/token`
        (`auth/play_integrity/{token,verdict}.rs`): local JWE decrypt (A256KW + A256GCM, protected
        header as AAD) → JWS verify (ES256) → verdict policy. Nonce bound to the request: the
        token's `requestDetails.nonce` must equal the shared `client_data_hash`
        (`sha256(challenge ‖ clientId ‖ sha256(body))`).
        Verdict checks are an exact port of legacy `validation.workflow.ts`: the three
        `PLAY_INTEGRITY_MODE` policy tables (`strict` / `relaxed_device` / `relaxed_all`), all
        five error codes collected (not first-fail), unknown wire verdicts treated as missing
        (legacy ACL), package header allow-listed before decoding (legacy middleware).
  - [x] (2026-07-25) Derive `appFromOfficialStore` from the signing-digest routing (token's
        base64url `certificateSha256Digest` vs the Play Store / website fingerprints; absent
        digests — tolerated only in `relaxed_all` — derive `false`). Note: legacy omits the JWT
        claim entirely on this path (consumers default `?? false`); V2 derives it per this plan.
  - [x] (2026-07-25) Config: reuses Phase 3's `ANDROID_PACKAGE_NAMES` +
        `ANDROID_SIGNING_DIGEST_{PLAYSTORE,WEBSITE}`; adds `PLAY_INTEGRITY_MODE` (default
        `strict`) and `PLAY_INTEGRITY_{DECRYPTION,VERIFICATION}_KEY` (validated at startup when
        set; deliberately **optional** while the ops exchange is pending — unset keys fall back
        posture-gated instead of blocking boot, so soft mode / other platforms are not
        held hostage).
  - [x] (2026-07-25) **Temporary Google-API fallback** (`play_integrity/google.rs`): when the
        self-managed keys are unset and `GOOGLE_CREDENTIALS` (legacy name/format: base64
        service-account JSON) is set, tokens decode via
        `playintegrity.googleapis.com/v1/{package}:decodeIntegrityToken` (RS256 assertion →
        cached OAuth token, same flow as the notifications FCM provider), then feed the **same**
        nonce binding + verdict policy. Self-managed keys take precedence the moment they are
        set. Coexistence verified against Google's docs: self-managed keys keep the Google decode
        path working; only switching **back** to Google-managed invalidates downloaded keys.
        **Delete this fallback (module, config var, deps) once the exchange lands** — it carries
        the 10k/day quota exposure and the Google runtime dependency V2 exists to remove.
  - **✅ Demo**: captured real-device integrity token verifies green; expired/foreign-package
    token rejected in hard mode, logged in soft mode. *(Synthetic-fixture tests cover the whole
    pipeline today — self-minted AES-256/EC-P-256 keys, byte-faithful JWE/JWS tokens, green paths
    for all three modes, and rejects for wrong keys, tampered signature, nonce mismatch, foreign
    package/digest, sideloaded shapes, alg confusion (`RSA-OAEP`, `alg=none`). The real-token
    demo can now run through the Google fallback with the legacy service account — no longer
    gated on the key exchange.)*
  - Risks: verification path decided (self-managed keys, see Key decisions); the **ops exchange
    is still open** but now only gates the local path, not the demo: server team generates an
    RSA-2048 pair, Android team uploads the public key in Play Console and returns the `.enc`
    bundle → decrypt → `PLAY_INTEGRITY_DECRYPTION_KEY` + `PLAY_INTEGRITY_VERIFICATION_KEY`
    secrets. Until then the fallback reintroduces the 10k/day quota exposure — acceptable
    temporarily, not for the hard-mode flip.

- [ ] **Phase 3 — Android hardware-keystore attestation (vanilla/no-GMS).**
      *(2026-07-25: code-complete + golden-fixture tested — both real captured chains
      verify green; ✅ demo still needs a fresh vanilla-build capture against the
      live deployment.)*
  - [x] (2026-07-25) Verify the chain the shipping vanilla flavor sends on `/auth/token`:
        dispatch on `Auth-Attestation-Type: key-attestation` (case-sensitive protocol enum),
        read the `attestationChain` field from the request **JSON body** (base64 DER array,
        leaf first, 2–10 entries ≤8192 chars each — legacy zod bounds; the legacy backend
        pins "It is NOT a header"). Parsed from the same raw bytes the sr25519 proof hashes
        (`key_attest.rs::chain_from_body`).
  - [x] (2026-07-25) Verification (`auth/key_attest/{verify,extension,crl}.rs`): X.509 chain to
        the hardcoded Google roots (RSA + EC; root-first order tolerated like legacy), CRL check
        against `android.googleapis.com/attestation/status` (1h cache, stale-snapshot fallback on
        failed refresh, hex + decimal serial spellings), hand-rolled DER parse of the
        key-description extension (OID `1.3.6.1.4.1.11129.2.1.17`, leaf-only), TEE/StrongBox
        security level for both attestation and key, root-of-trust policy (locked bootloader;
        `Verified`, or `SelfSigned` pinned to the GrapheneOS verified-boot fingerprints — that is
        what makes GrapheneOS first-class), challenge match, package ∈ `ANDROID_PACKAGE_NAMES`,
        signing digest ∈ {Play Store, website}. Shared X.509 signature/validity primitives
        extracted to `auth/x509.rs` (used by App Attest too); Android policy stays separate.
  - [x] (2026-07-25) `appFromOfficialStore` from the signing-digest routing (Play digest → true,
        website digest → false, unknown/mixed → reject); iOS keeps the hardcoded App Store
        contract. Config: `ANDROID_PACKAGE_NAMES` + `ANDROID_SIGNING_DIGEST_{PLAYSTORE,WEBSITE}`
        (legacy names, colon-form accepted, fail-fast when `AUTH_ENABLED=true`),
        `ANDROID_ATTESTATION_CRL_URL`, `ANDROID_ATTESTATION_CRL_CACHE_TTL_SECS`.
  - **✅ Demo**: the two real captured chains (a real
    Google device with its `Auth-Challenge`, and a GrapheneOS nightly build) verify green through
    the full pipeline; software-level attestation and CRL-revoked certs rejected in hard mode.
    *(Golden fixtures committed under `crates/identity-service/testdata/key_attest/`; verifier
    pins 2026-06-10 because the embedded Droid CA3 / TEE intermediates are short-lived. 18 unit
    tests cover both captures plus every reject reason — challenge, package, digest, revocation,
    expiry, untrusted root, untrusted verified-boot key, software level, unlocked bootloader.
    Remaining gate: a fresh capture from a vanilla-flavor build against the live deployment.)*
  - Risks: the fixture chains have short-lived intermediate certs (Droid CA3 rotates
    ~biweekly) — the fixture verifier must pin the verification time, and fresh captures need a
    vanilla-flavor build.
    ~~GrapheneOS root question~~ **resolved**: the captured GrapheneOS chain roots to Google's
    standard `Key Attestation CA1` (hardware attestation is Google-provisioned even on
    GrapheneOS) — no extra root CA needed (confirmed against the capture: the chain verifies with
    only the pinned Google roots, and its `SelfSigned` verified-boot key is the GrapheneOS
    Pixel 10a fingerprint).

- [ ] **Phase 4 — Apple DeviceCheck (iOS uniqueness / PoUD).**
      *(2026-07-25: client + gate decision shipped; the username-claim wiring landed once the
      concurrent `POST /usernames` rewrite merged.)*
  - [x] (2026-07-25) Server-to-server DeviceCheck client (`src/device_check/{client}.rs`):
        cached ES256 team-key JWT (`kid` header, `iss` = `APPLE_TEAM_ID`, 10-min TTL, 30s
        refresh grace — legacy `.p8` names `DEVICE_CHECK_KEY_ID` / `DEVICE_CHECK_PRIVATE_KEY`,
        `DEVICE_CHECK_URL`), `query_two_bits` / `update_two_bits` with base64 token +
        transaction id + ms timestamp, 5s timeout. Legacy two-bit encoding preserved: "used" =
        `!bit0 && bit1`, register writes `(false, true)`, plus the admin-style reset
        (`(false, false)`) for test devices. A 200 with a non-JSON body is a **fresh device**,
        not an error. Config gated by `DEVICE_CHECK_IOS_ENABLED` (default off; fail-fast keys
        when on); client lives on `AppState`.
  - [x] (2026-07-25) Posture handling as a pure gate (`device_check::decide_gate`, straight
        port of legacy `gate.workflow.ts`): disabled → proceed (no verdict); soft → always
        proceed with an advisory `available`; hard → Register / Blocked / TokenRequired /
        Unavailable. Apple outage in soft mode proceeds with no verdict — never a 500.
  - [x] (2026-07-27) Wire into the username-claim decision (mirror legacy
        `device-check/gate.workflow.ts`): bit set → `PAYMENT_REQUIRED` outcome; unseen device →
        mark used + proceed — **not an error** (200 with `registrationOutcome`). The hard-mode
        free-iOS claim is serialized by a transaction-scoped Postgres advisory lock and
        **re-queries Apple under the lock** before marking, so concurrent requests cannot
        double-spend the free slot (the gate's pre-transaction query is stale by then). A losing
        contender resolves `PAYMENT_REQUIRED`. Residual: Apple's mark is external, so a commit
        failure after a successful mark can consume the slot without a durable reservation — it
        fails safe (device loses its free slot, never gains an extra). The client retries transient
        Apple failures.
  - **✅ Demo**: first registration from a device token passes and flips the used bit; a second
    registration from the same device resolves `PAYMENT_REQUIRED`; DeviceCheck API outage does
    not 500 the route (posture-dependent fallback, matching legacy `device_check_available`).
    *(Client-level: JWT shape/signature, bit-state semantics, gate table, and fresh-device
    response parsing are unit-tested; the route demo needs a real device token. The concurrent
    double-spend guard (advisory lock + under-lock re-query) needs a live-Postgres + mock-Apple
    integration test, deferred to the live-Postgres suite alongside the other parked DB tests.)*
  - Risks: **shares the `POST /usernames` handler** with the concurrent rewrite; sequence this
    phase after that rewrite merges and coordinate the decision-flow insertion point.

- [ ] **Phase 5 — Enforcement flip + release-device smokes.**
  - [ ] Enable `AUTH_ENABLED=true, ENFORCE_AUTH=false` (soft) on `identity.dotspark.app`; watch
        verdict logs across real iOS + Android release builds.
  - [ ] Flip `ENFORCE_AUTH=true` (hard) once soft-mode verdicts are clean.
  - [ ] Update `docs/architecture.md` (remaining-scope item 1), `.env.example`, runbook, and
        the API reference (`just openapi`) — same change as the code, per AGENTS.md.
  - **✅ Demo**: release iOS and Android apps complete challenge→attestation→JWT→registration
    against the hard-gated live deployment; a curl without attestation gets `403
    INTEGRITY_FAILED`.

# Contracts (guardrails)

- The mobile wire is frozen: paths, headers, and JSON shapes stay byte-compatible with the
  shipping clients. Attestation adds *verification only* — **no new routes** (all three
  genuineness paths ride the existing `/auth/app-attest/attestations` + `/auth/token`).
- Wire facts verified against the shipping clients (2026-07-24): iOS sends the App Attest
  assertion base64 in `Auth-Payload` **plus the key handle in `Auth-iOS-KeyId`**; Android gp
  sends the Play Integrity token in `Auth-Payload`
  with nonce = `sha256UrlSafe(challenge ‖ clientPubkey ‖ sha256(body))` — the backend must
  recompute exactly this nonce; Android vanilla sends `attestationChain` in the body.
- `AUTH_ENABLED=false` must remain byte-identical to today's behavior (the M0/testnet mode and
  the `.local` E2E runbook must keep passing).
- The sr25519 account proof stays mandatory in every posture — attestation is additive, never a
  substitute.
- Attestation failure handling is posture-gated: soft mode logs the verdict and never rejects
  (this includes `/auth/app-attest/attestations`, which soft-accepts `202 {}` without storing the
  unverified key), and a soft-mode failure issues the JWT with `appFromOfficialStore=false` —
  never a fabricated `true` (the hardcoded `true` survives only in the `AUTH_ENABLED=false`
  no-op).
  **Hard-mode status codes (decided 2026-07-28, see Deviations):** a failed platform-attestation
  verdict on `/auth/token` returns **`403 INTEGRITY_FAILED`** (the device/app failed the genuineness
  check — not a `401`, which stays for a bad challenge/spent-challenge or invalid client proof). An
  Android attestation **CRL outage** in hard mode returns **`503 ATTESTATION_CRL_UNAVAILABLE`**
  ("retry with a fresh challenge") — an infrastructure failure, not a device failure; soft mode still
  logs and allows. A refresh failure keeps serving the last snapshot, but only up to a **1-hour
  maximum staleness** (`ANDROID_ATTESTATION_CRL_MAX_STALE_SECS`, spec SC-004); past that the snapshot
  is refused (the same `503`), so a since-revoked key can't stay accepted through a long outage —
  bounding the legacy "serve stale forever" behavior. **Caveat:** V2 emits its standard `{error, message}` envelope with these codes;
  the exact legacy `{_tag, error}` body shapes (e.g. `AndroidAttestationCrlUnavailable`,
  `VERIFY_ATTESTATION_FAILED`) are **not yet golden-captured** — capture them and reconcile the body
  before the Phase 5 flip, and confirm the shipping client handles `403`/`503` on this path. The
  `/auth/app-attest/attestations` registration endpoint still returns its legacy `401 {_tag}` bodies.
- DeviceCheck "seen device" is a **200 `PAYMENT_REQUIRED` outcome, not an error**.
- All of this stays inside `identity-service` — no new crate/service (architecture guardrail).

# Deviations from Mike's spec (decided 2026-07-28)

Where V2 diverges from Mike's [New JWT / Integrity / PoUD/Username Claim Logic] doc, decided
deliberately. Recorded so a reviewer diffing against the doc knows these are intentional, not bugs.

1. **Signing digests — check only the first (`signatureDigests[0]`).** Mike checks the first; earlier
   V2 required *every* attested digest to agree on one channel. **Decision: match Mike (first-only).**
   Our builds are single-signer, so the list holds one digest; extra digests (multi-signer / signing-key
   rotation) are ignored. *If we ever rotate the Android signing key, add the new digest to config
   before the overlap.* (`key_attest/verify.rs::check_policy`.)
2. **Root-of-trust — stricter than the doc, kept.** V2 also enforces locked bootloader +
   `Verified`/GrapheneOS-`SelfSigned` boot state + key security level. These are independent of the
   security level (a rooted device still reports TEE), so they are the checks that actually catch a
   tampered device. Legacy-backed; keep.
3. **Chain length `2..=10` vs Mike's 3–5.** Length is a DoS sanity bound, not a trust control —
   root-pinning + per-cert signature verification do the real work. Wider bound is more robust to
   vendor variation. Keep.
4. **Play Integrity nonce always required.** Legacy skips the nonce when the body is empty; V2 always
   requires and matches it (closes a replay bypass). Our `/auth/token` requests always carry a body.
   *To verify in the soft-mode smoke: the shipping client always sends a body on this path.*
5. **Error status codes** (see Contracts): `403 INTEGRITY_FAILED` for a failed attestation verdict
   (semantically correct + Mike's target), `503 ATTESTATION_CRL_UNAVAILABLE` for a hard-mode CRL
   outage. Exact legacy `{_tag}` body shapes still need golden-capture before the flip, and the
   `403`/`503` client handling must be confirmed.
6. **Voucher attestation type — dropped from this slice.** Legacy's `Auth-Attestation-Type: voucher`
   (a single-use DB-backed bypass of platform attestation) is rejected as unknown. Reviving it is a
   **separate follow-up** (see Later), not a byte-compat gap to close now.
7. **Success shape — `202` reservation, not Mike's universal `200 {registrationOutcome}`.** V2 keeps
   legacy's `202` reservation body for a successful claim (registration is async: reserve → chain
   writer, so `202` is also the honest code) and only returns `200 {registrationOutcome:
   PAYMENT_REQUIRED}` for a seen device. Full unification to one `200 {registrationOutcome}` model
   belongs to the later eligibility engine (remaining scope #2).

# Key decisions (decide early)

- **Play Integrity verification path — decided: self-managed decryption keys.** The spec
  documents the rationale (the 10k/day
  `decodeIntegrityToken` quota is a DoS vector), the token structure (JWE A256KW+A256GCM
  wrapping a JWS ES256 verdict), and the key-exchange runbook. V2 verifies offline with
  `DECRYPTION_KEY`/`VERIFICATION_KEY`; kick off the Play Console key exchange with the Android
  team early — it's Phase 2's only external dependency.
- **App Attest key ↔ account binding — decided (2026-07-24): mirror legacy (free-floating).**
  Keys are stored by `key_id` alone; any registered key can assert for any account. Rationale:
  byte-compat is the mission; the App Attest key is per-install (dies on uninstall) while the
  account restores from iCloud Keychain, so binding would break reinstall/restore and
  logout-to-new-account flows; the per-request link already exists cryptographically (the
  assertion signs challenge + clientId + body); one-device-many-accounts abuse is uniqueness'
  job and is handled by DeviceCheck at username claim (Phase 4). Record the clientId observed at
  registration for observability only — never enforced. Strict binding is a post-cutover
  hardening candidate (would need re-bind/unbind semantics).
- **Rust dependency set** for the crypto: X.509/chain verification, CBOR, ES256/P-256, ASN.1
  extension parsing. Survey crates before Phase 1 (`x509-parser`, `ciborium`, `p256` are the
  likely set); no compile-time-heavy frameworks.
- **Fixture strategy**: attestation evidence expires and is device-bound — decide per-platform
  what is golden-captured (structure/negative cases) vs live-only (full chain freshness), so CI
  stays hermetic.

# Testing

- Per-phase captured-fixture replay through the router (same pattern as the turn/dim golden
  fixtures): real-device captures for pass cases, mutated captures for every reject reason.
- **The spec is a fixture source**: it carries two complete real key-attestation chains (Google
  device + GrapheneOS, with challenges) for Phase 3, and the per-environment signing digests
  (debug / nightly / Play internal / Summit website+Play) for config and both Android phases.
- Chain fixtures embed short-lived intermediates — the test verifier pins verification time
  rather than using wall-clock.
- No-op-posture regression fixtures pinning today's behavior byte-for-byte.
- `just check` green per phase; live smoke against `identity.dotspark.app` at each posture flip.

# Later / Deferred

- **The two-step `POST /auth/android/attestation` endpoint** (chain → single-use
  `attestationToken` → `Auth-Attestation-Token` at `/auth/token`) — a proposed redesign no
  client or backend implements today. Additive to Phase 3 (same verification core, different
  transport); adopt only if the client team builds against it.
- **Auth voucher — planned follow-up (decided 2026-07-28: we will want it).** V2 rejects
  `Auth-Attestation-Type: voucher` today. Rebuild it as a first-class feature (not the legacy debug
  route): a voucher table + a dispatch branch on the shared attestation core + a real minting path.
  Own it as its own slice, not part of this attestation PR. Flag the legacy-backend owner before
  cutover in case anyone still hand-mints vouchers via manual DB inserts.
- **Pre-Phase-5 dialect capture** — golden-capture the exact legacy hard-mode error bodies
  (`{_tag, error}` shapes) and reconcile V2's `{error, message}` envelope before flipping
  `ENFORCE_AUTH=true`.
  **Client 403/503 handling confirmed 2026-07-28** (against synced upstream iOS `develop` /
  Android `master`): the change is **non-breaking** — neither client routes `/auth/token` failures
  through its 401 token-refresh path (iOS retries 401 only on downstream authorized calls; Android's
  OkHttp `BearerTokenAuthenticator` is bound to the separate `@BearerAuth` client), so 403 will not
  wrongly trigger re-auth, loop, or crash. Both treat 403/503 as a **generic** token-acquisition
  failure: iOS recognizes 401/503 but not 403; Android maps any non-2xx to a generic `HttpException`.
  Neither retries on 503 or surfaces the `message`. **Optional (not required) UX patch** for a clean
  rejection-vs-retry experience: iOS add `case forbidden = 403` (+ optional 503 retry); Android add
  status/body-aware mapping around `JWTTokenProvider.fetchTokenViaAttestation()` (keep 403 off the
  401 authenticator).
- **Android PoUD signals** (`androidId`, `widevineId` OR-check from the spec) — revisit when
  the eligibility/queue plan lands and if Android uniqueness is prioritized despite the spec's
  "no proper mechanism" stance.
- **QR registration voucher + queue/payment engine** — eligibility plan (remaining scope #2),
  where the DeviceCheck outcome plugs in.
- **Play Integrity self-managed keys** (if Phase 2 picks the Google-API path first) — **done
  inverted**: the self-managed path shipped first; the Google-API path exists as the temporary
  fallback. The open item is the reverse: **remove the `GOOGLE_CREDENTIALS` fallback**
  (`play_integrity/google.rs`, config var, `jsonwebtoken` dep) once the Play Console key
  exchange lands and the self-managed keys are deployed.
- **Attester quorum / decentralized attestation** — design-doc thread, post-cutover.
- **Proof-of-compute gate** — Future Work per the design doc.
