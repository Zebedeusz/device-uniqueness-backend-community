# Identity Backend V2 — Architecture & Naming Alignment

> Current architecture, scope, service boundaries, naming, and data-flow invariants for humans and
> agents. Code and tests remain the source of truth; update this file in the same change whenever
> implementation changes make it inaccurate.
>
> **No delivery status here.** Whether a slice is in flight, demoed, or shipped lives in
> `docs/plans/` (`active/` vs `done/`) — this file describes the system as designed, so it does not
> go stale every time a phase lands.

## Vision

Identity Backend V2 replaces the coupled TypeScript backend with **one Rust Cargo workspace of
small, independently-deployable services under one team**. Boundaries exist to isolate secrets,
persistence, deployment lifecycle, and failures: a notification, TURN, invite, or indexing fault
must never disrupt login, attestation, or username registration.

Principles that drive every boundary decision:

- **`identity-service` is the sole JWT issuer.** Siblings are *verify-only* — they validate its
  asymmetric JWT via JWKS **when a route needs auth**, but never mint tokens, never call
  identity-service synchronously, and never read its tables. *Verify-only does not mean every
  route requires a JWT* — e.g. username search is public.
- **Shared code is minimal and product-agnostic.** No shared database, no shared product logic.
- **Per-service persistence.** Each stateful service owns its data; stateless services add none
  without evidence. No shared tables.
- **The mobile API is frozen** — same base URL, same paths/payloads — behind one path-routing
  front door. V2 rebuilds every live route and **fully retires V1**; two permanent backends is a
  failure state.

## Naming convention

**Plain, boring, descriptive names. No decorative prefixes.**

- A name says what the thing **is/does** in plain English — a new engineer guesses the role in <1s.
- **No workspace prefix** (`id-`, `shared-`, `svc-`, `lib-`). The repo is already the namespace;
  `Cargo.toml` and `docker ps` show the plain name. The non-identity services (`notifications`,
  `turn`, `invites`) are *not* identity — don't imply otherwise.
- Lowercase kebab-case, short, greppable. Boring is good.
- **No themed/metaphorical names** (no `mint`/`citadel`/`signet`/nautical/etc.) and **no invented
  `-ity` brands** (the `-ity` policy is public-facing only and exempts crate/CLI names; also avoids
  colliding with Trinity/Levity/Nominality/…).

## Naming vocabulary

This is the **agreed vocabulary** for parts as they come to exist — **not** a list of folders to
create now. See "How the workspace grows" below.

### Shared library crates (product-agnostic, imported by services)

| Concept | Name | Where it lives |
|---|---|---|
| generated People Chain (subxt) typed surface | `chain-types` | crate; consumed by identity-service, username-indexer, dim-tickets, invite-tickets |
| chain connection / RPC transport + writer signing key | `chain-client` | crate (reconnecting connect + `WriterSigner` + `proxy_target`) |
| Postgres connection + bootstrap | `postgres` | still a module inside identity-service / username-indexer |
| config loading + fail-fast validation | `config` | still a module per service |
| telemetry (logs / metrics / tracing) | `telemetry` | `http-common` (`telemetry::init` installs the log subscriber, `metrics::spawn` the Prometheus exporter); every process calls both. Logs go to stdout only — labelling, shipping, storage, and retention belong to the host's monitoring project, not to a service |
| JWT + JWKS verification (for verify-only services) | `jwt-verify` | crate; the cross-service auth contract — identity-service delegates its own verification to it; dim-tickets, invite-tickets, turn, notifications, and username-indexer (for the proof-of-compute bypass) consume it |
| frozen legacy HTTP dialects + axum scaffolding (problem details / Hono text bodies / zod v4 messages, JWT extractor, rate limiter, health, middleware stack, env helpers) | `http-common` | crate; consumed by dim-tickets, invite-tickets, turn, notifications. The dialect bodies are golden-captured — change only with a fresh capture |

### Deployable services (plain domain word)

| Service | Responsibility | Processes |
|---|---|---|
| `identity-service` | Device **attestation** + auth handshake + **sole JWT issuer** + username **write** path + **eligibility/queue** + reservation **outbox**. | `identity-api` (N replicas) + `identity-chain-writer` (single-instance outbox→chain worker) + `registration-queue` (single-instance queue advancer; with `QUEUE_ENABLED` on its promotion is the only queue exit — down, claims park as `QUEUED` behind the free lane's throttle and the writer raises the stranded-queue warning) |
| `username-indexer` | Finalized-chain username **indexer + public reads**: prefix search (`GET /api/v1/usernames/search`, paginated, per-IP rate limited) plus the optional proof-of-compute gate and its `POST /api/v1/poc/issue` puzzle issuance. The single-username lookup (`GET /api/v1/usernames/{username}`) is retired in favour of `search` and serves the Hono 404, like the sunset list endpoint. Read-only projection; can be down without affecting registration. | `username-indexer` (+ own Postgres) |
| `notifications` | Thin `/api/v1/notify` relay. Verify-only Ed25519 JWT, stateless, DB-free, per-subject rate limited. No `depends_on`. iOS APNs (token auth, HTTP/2) + Android FCM (v1, OAuth2) providers, each optional. | `notify-relay` |
| `turn` | JWT-gated `POST /api/v1/turn/issue` — stateless coturn REST-API credential issuer (username = expiry:id, password = HMAC over the username, secret shared with the relay; the relay itself = SRE infra). No DB, no chain; N replicas need no coordination. | `turn-api` |
| `invite-tickets` | JWT-gated `POST /api/v1/invitation-ticket/claim` — synchronous invitation-credential claims from a pre-staged, on-chain-registered sr25519 keypair pool (its own Postgres). This is the route the shipping apps call for Game / ProofOfInk DIM claims. | `invite-tickets-api` (N replicas, DB-only, no signing secret) + `invite-tickets-pool` (single-instance keypair generator + on-chain registrar) |
| `dim-tickets` | JWT-gated `/api/v1/dim-ticket` — DIM (Game / ProofOfInk) invitation-credential issuance + status. Own Postgres state machine. No known client consumer — the apps use the `invite-tickets` claim route. | `dim-tickets-api` + `dim-tickets-writer` (single-instance: `Utility.force_batch` of `set_invite_ticket`, per-item event split, retry/orphan recovery, `writer_lease`) |
| `gateway` (edge) | Public-URL routing via **Caddy**, one site block per environment. The route table is the committed `gateway/Caddyfile`: the `(routes)` snippet is imported once per environment with that environment's upstreams — username GET reads + `/api/v1/poc*` → its `username-indexer`, DIM tickets → its `dim-tickets-api`, invitation tickets → its `invite-tickets-api`, TURN → `turn-api`, notify → `notify-relay`, everything else (identity-owned writes/availability/root preflight, JWKS, health) → its `identity-api`. Upstreams are container aliases suffixed with `ENV_ID` (`identity-api-paseo-next-v2`), resolved over the shared external `ibv2-edge` network; an environment that does not run a service points that route at its own `identity-api` so it 404s. Shared services (`turn`, `notify`) exist once and verify every environment's tokens from a merged JWKS. This is the **only** container publishing host ports (80/443, plus 443/udp); holds no secrets (both enforced by the compose boundary script). | `caddy` in its own compose project (`-p edge`) |


## How the workspace grows (lazy extraction)

The vocabulary above is **not** "make 11 folders." Folders appear incrementally:

- The crate list as it stands today is in `AGENTS.md` → Workspace.
- **Services** become crates **when you build them** — a service gets a folder the day work
  starts, not before.
- **Shared libs are extracted lazily — only when a *second* service needs the code.** Until then
  the logic lives as **modules inside** the owning service (today: `identity-service/src/{config,db,chain}.rs`).
  `jwt-verify` was the first such extraction — pulled out the day `notifications` became the second
  verify consumer. Don't create a shared crate speculatively.
- **When you do extract, prefer a *few focused* crates, not extremes:**
  - ❌ 6 tiny crates scaffolded up front (premature boundaries, churn).
  - ❌ one `common`/`shared` grab-bag (every service recompiles + pulls everything).
  - ✅ a handful (realistically **3–4** over time) that each do one clear thing. `chain-types`,
    `jwt-verify` (the cross-service auth contract), and `chain-client` (reconnecting transport +
    writer signing key) are shared today; next candidate is optionally a small `platform` grouping
    of `config`+`telemetry`(+`postgres`) **only if** they always travel together.

## Classification quick-reference

- **attestation** → inside `identity-service` (NOT a service, NOT a shared lib).
- **availability** → `identity-service` (People Chain plus durable reservation-outbox read).
- **username write** → `identity-service`.
- **username read/search** → `username-indexer` — true carve-out.
- **proof of compute** → `username-indexer` (the gate protects its public read; issuance lives with it).
- **free-reg eligibility/queue** → `identity-service` product logic.
- **chain-writer** → worker (`identity-chain-writer`) *inside* the `identity-service` boundary.
- **notifications / turn / invites** → independent thin services, each outside the identity
  boundary.
- **DIM tickets (game/personhood)** → the `/api/v1/dim-ticket` routes live in the `dim-tickets`
  service — not `identity-service`. But the apps' game/personhood **claims** go through
  `/api/v1/invitation-ticket/claim`, which is the `invite-tickets` service.
- **gateway** → Caddy path routing (infra).

### Guardrails — do NOT create these as separate crates/services

`attestation`, `availability`, `eligibility`, `queue`, `payment`, `writer` all live **inside**
`identity-service`. Splitting any out contradicts the design (single identity failure/secret boundary).

## Open decisions (need the user)

1. **Database topology** — separate instances vs databases vs schemas. ("No shared tables" already decided.)
2. **Migration/cutover** — migrate V1 data or start fresh, then move each route's clients onto this backend + V1 shutdown. (The Caddy-level legacy-origin lane was removed on 2026-08-03; nothing was ever configured behind it, so a cutover means pointing clients at a route that has a Rust owner.)

## Remaining scope & legacy retirement

Remaining slices. Each gets its own ship-first plan under `docs/plans/` when work begins; that plan
(not this list) tracks how far it has got:

1. Device attestation inside `identity-service`: iOS App Attest, Android hardware-keystore/TEE
   attestation, Android Play Integrity (self-managed keys), Apple DeviceCheck, and the enforcement
   flip.
2. Registration eligibility, payment, and voucher behavior: the INSTANT / PAYMENT_REQUIRED lanes,
   the `payment-status` endpoint, and the spec's `200 {registrationOutcome}` claim envelope.
3. Production hardening: real batching if measurements require it, load tests, metrics, alerts, and parity benchmarks.
4. iOS and Android release-device validation.
5. Data migration, rollback, per-route client cutover, and legacy shutdown.

The legacy backend is retired only after every required route has an owner, the gateway is validated, parity gates pass, rollback is proven, and release-device smokes succeed.

## Implementation notes (data flow & invariants)

Operational invariants an agent must respect when touching the code.

- **Registration is an outbox.** `POST /api/v1/usernames` inserts a `RESERVED` row in
  `username_reservations` and returns `202`; `identity-chain-writer` claims rows and submits the
  People Chain calls (`PeopleLite.attest` / `Resources.register_lite_person`), advancing
  `RESERVED → SUBMITTING → ASSIGNED | RETRY_AFTER | FAILED_TERMINAL`. The DB row is source of truth;
  chain is reconciled to it. No service reads another service's tables.
- **The registration queue is an outbox entry state, not a second pipeline.** With
  `QUEUE_ENABLED` (default off), `POST /api/v1/usernames` inserts claims as `QUEUED` rows with a
  `queue_group` (1–4) derived from the JWT subject's People Chain free balance (<10 DOT → 1,
  ≥10 → 2, ≥100 → 3, ≥1000 → 4; fail-open to 1). The advancer — the standalone single-instance
  `registration-queue` service — refreshes groups from chain and promotes up to 4 rows to
  `RESERVED` per iteration (`QUEUE_ADVANCE_INTERVAL_SECS`): slot k serves groups ≥ 5−k,
  earliest-enqueued wins, empty slots are skipped. Promotion is the only `QUEUED` exit; the
  writer path is unchanged from `RESERVED` on. `GET /api/v1/registration/queue` (JWT) reports
  position/group/ETA by simulating that drain over the current snapshot; the claim response
  gains additive `registrationOutcome: "QUEUED"` + `queue` fields (the frozen queue-disabled
  wire is untouched).
- **A down queue service parks claims; it never unthrottles them.** The queue is the free
  lane's throughput control, so `QUEUE_ENABLED` alone is the policy: while it is on, every
  free-lane claim inserts as `QUEUED` and advancer promotion is the only queue exit. The
  `registration-queue` advancer holds a `writer_lease` row (`registration-queue-advancer`)
  that doubles as its liveness signal and single-instance guard — but liveness only affects
  *progress*: a dead (or never-deployed) advancer leaves claims parked durably behind the
  throttle, drained in fair slot order when it restarts, and the `identity-chain-writer`
  raises a stranded-queue warning (paced by `QUEUE_FALLBACK_AFTER_SECS`) as the operator
  signal. With `QUEUE_ENABLED` **off**, the writer is the janitor: once the advancer lease
  has been expired longer than `QUEUE_FALLBACK_AFTER_SECS` (default 60) it drains leftover
  `QUEUED` rows to `RESERVED` itself (an **absent** lease row drains immediately — with the
  queue disabled nothing else would ever promote them), so retiring the queue strands
  nothing. `lease::renew` refuses an already-expired lease, so a stalled-then-recovered
  advancer re-acquires a fresh epoch instead of silently resurrecting a lease the janitor
  may have acted on; during long iterations the advancer renews concurrently with the work.
  `identity-api` and `identity-chain-writer` read the **same** `QUEUE_ENABLED` value and
  must never be deployed split: a mismatch either strands rows (writer on, api off) or
  drains a queue the api is still filling (writer off, api on).
- **Flipping `QUEUE_ENABLED` off is safe with rows still queued, but degrades visibility.**
  The flag gates only intake and the status route's mounting (`/api/v1/registration/queue`
  unmounts → plain-text 404; clients are documented to treat any 404 as "no longer queued,
  registration proceeding"). Promotion never consults the flag: a live advancer keeps draining
  queued rows, and a stopped one hands off to the writer's janitor drain (which runs only
  while the writer's own flag is off) — so nothing strands, but users mid-queue lose their
  standing display (and a rolling flag flip serves mixed answers across replicas). Preferred
  sequence when retiring the queue: flag off everywhere, writer last (intake stops first),
  let the advancer drain the remainder, then stop the advancer. Known
  API-shape limitation: an account with several queued claims sees only the earliest one's
  standing (the spec's response is a single object).
- **Registration is authorized by `candidateSignature`, not the JWT subject.** `POST
  /api/v1/usernames` verifies the beneficiary's sr25519 signature over `pop:people-lite:register
  using || candidatePubkey || ringVrfKey` (the message the chain reconstructs for `PeopleLite.attest`;
  Substrate signing context) before enqueuing. The JWT authenticates only the device/session — a
  separate per-install auth wallet — so `candidateAccountId` is deliberately **not** required to
  equal the JWT subject. Stored `account_id` (JWT subject) is the abuse-control/rate-limit key;
  `candidate_account_id` is the on-chain beneficiary the writer submits for.
- **`identity-chain-writer` is single-instance by design** — a nonce lane is **(signing account,
  chain)**, and one process holds one lane per chain. The `writer_lease` table is a best-effort
  deploy-overlap guard only; the chain nonce + outbox reconciliation is the real serializer.
  Never run two writers / never scale it. The writer owns **two** lanes on one account, People
  and Asset Hub. That is safe because nonces are per chain. Both sit behind the same single
  lease, so one lease still means one submitter on each.
- **The dotNS gateway lane is a second state machine on the same row.** Since
  paritytech/individuality#755 there are two independent username stores. People
  `Resources::UsernameOwnerOf` is written by `PeopleLite.attest`. The Asset Hub dotNS contracts
  are written **only** by `DotnsGateway::reserve_name`. There is no bridge between them.
  A row therefore carries `status` (People) and `dotns_status` (Asset Hub). The two advance
  independently through `PENDING → SUBMITTING → RESERVED | RETRY_AFTER | FAILED_TERMINAL |
  EXPIRED`. **A dotNS failure never changes `status`.** `ASSIGNED` + `DOTNS_FAILED_TERMINAL` is a
  legitimate resting state: the username works, the dotNS name does not. The dependency runs one
  way only: a People `FAILED_TERMINAL` closes an open dotNS half as `ABANDONED`, since a name with
  no username behind it can never be submitted. Ordering is not free.
  `reserve_name` writes `LiteLabelOwner`, a global claim on the label. Asset Hub is therefore
  attempted **only** for rows already `ASSIGNED` on People. `dotns_status` `NULL` means the
  request carried no `dotns` block *or* the row predates the lane. There is no backfill, so
  pre-existing rows are never submitted.
- **The gateway lane's freshness bounds come from the chain, not from config.** `reserve_name`
  enforces `MaxValiditySeconds`/`MaxFutureSkewSeconds` against the client's `signedAt`, and the
  writer enforces **both** before spending an extrinsic — identity-api's
  `DOTNS_MAX_FUTURE_SKEW_SECS` is a separate value on a separate process and cannot be the only
  guard. The backend **cannot re-sign**; only the client holds the candidate key. An aged-out row
  is `EXPIRED`: terminal, never retried. A future-dated one is *deferred* behind
  `dotns_not_before` without spending an attempt — the clock resolves it, so failing it would kill
  a row that was always going to succeed.
- **A dotNS problem parks the dotNS lane; it never stops the writer.** Only one dotNS condition is
  a startup abort: `DOTNS_GATEWAY_ENABLED` on with no `ASSET_HUB_RPC_URL`, a config error knowable
  before any row is claimed. Everything else is runtime. Asset Hub is connected lazily on the
  first pass, not at boot, so an unreachable endpoint — an RPC bounce, a DNS blip, maintenance —
  leaves rows in `PENDING` and keeps People attesting, instead of crash-looping the process. The
  `DotnsGateway::reserve_name` shape assertion sits on that same path: the live Asset Hubs
  genuinely disagree on this call, and a runtime upgrade can start failing it at any time, so it
  is asserted per connect and parks the lane rather than the writer. `ibv2_dotns_lane_connected`
  is the signal.
- **Signing:** one canonical `ATTESTER_ACCOUNT` (SS58) names the attester authority, and **proxying
  is derived, not configured**: the writer signs `PeopleLite.attest` directly when that account is
  its own signer, and wraps it in `Proxy.proxy(real = authority, …)` otherwise. A proxied call's
  real outcome is `Proxy.ProxyExecuted`, not the extrinsic's — the outer extrinsic succeeds even
  when the inner call is rejected — so the writer inspects `ProxyExecuted` before advancing a row to
  `ASSIGNED`, recording any rejection as `Pallet::Variant` (resolved through the vendored metadata).
  `ASSIGNED` therefore always means the registration is on chain.
- **identity-api and identity-chain-writer read the same `ATTESTER_ACCOUNT`.** Clients fetch
  `GET /api/v1/attester` and bind their consumer-registration signature to it, so the published key
  and the attesting account are one value by construction and cannot drift into
  `PeopleLite::InvalidAttestationSignature`. The API derives the `0x`+hex wire form; nothing
  configures it separately. Both pallets see that same account as the attester — under
  `Proxy.proxy(real = P)`, `P` is what `GET /api/v1/attester` returns, on People and Asset Hub
  alike.
- **`chain-types` is the only place chain types live.** Static codegen from vendored metadata at
  `crates/chain-types/metadata/people.scale`; online transport + signing live in
  `identity-service::chain`, never in `chain-types`. Regenerate with the `subxt metadata …` command
  at the top of `crates/chain-types/src/lib.rs`. It holds one subxt config per chain family,
  `PeopleConfig` and `AssetHubConfig`. Their transaction-extension sets differ, and a merged
  tuple would exceed subxt's 26-member ceiling. **Only People is code-generated.** The Asset Hub
  surface is one extrinsic and two storage reads, built dynamically by name rather than by
  vendoring a second, faster-churning metadata blob.
- **HTTP** (`identity-service::http`): router in `http.rs` wrapped in a tower-http stack
  (request-id → trace → 30s timeout) with graceful shutdown. Health at `/livez`, `/readyz` (gates on
  a live Postgres `SELECT 1`), `/healthcheck`; JWKS at `/.well-known/jwks.json`. `AppState` =
  Postgres pool + `ChainClient` + `Jwt` + `Config`. `username-indexer` uses the same tower-http
  stack, but its `/readyz` also reports index freshness and it never publishes JWKS — it holds
  **verify-only** key material (`JWT_JWKS_JSON` / `JWT_ED25519_PUBLIC_KEY`), and only when
  `POC_ENABLED=true`, so authenticated callers can bypass the proof-of-compute gate.
- **invite-tickets: the pool has two states and the claim is one transaction.** A ticket row is
  `available` (generated, registered on-chain) or `claimed` — nothing else; failed registrations
  are never inserted. The claim is: pre-check count (`0` → 422 `Pool exhausted`) → one transaction
  `SELECT … FOR UPDATE SKIP LOCKED ORDER BY created_at LIMIT 1` + flip to `claimed` (no row →
  409 `Ticket race lost`) → sign → post-claim count as `remaining`. The signature is sr25519 by the
  **ticket** key over the **decoded 32-byte account id** of `who` (substrate signing context), not
  the address string. Pools are keyed `(dim, network)`; FIFO by `created_at`. The api bin never
  touches the People Chain and holds no signing secret besides the pool rows it serves — chain
  RPC and the inviter secret belong only to the single-instance `invite-tickets-pool` bin.
  **`invite-tickets-pool` and `dim-tickets-writer` must not sign as the same account** — two
  independent submitters on one account race nonces; give each its own inviter (e.g. separate
  proxy delegates of one cold primary) before deploying both. As for the attester, proxying is
  **derived, not configured**: `INVITER_ADDRESS` names the account holding the invites, and a
  signing key whose own account differs from it wraps the batch in
  `Proxy.proxy(real = INVITER_ADDRESS, force_proxy_type = Any)`.
- **DIM tickets are a per-row state machine, not an outbox copy.** `POST /api/v1/dim-ticket`
  quota-checks the inviter's on-chain `AvailableInvites` synchronously, then inserts a `PENDING`
  row (200); `dim-tickets-writer` batches due rows into one `Utility.force_batch` of
  `set_invite_ticket` calls and splits the ordered `ItemCompleted`/`ItemFailed` events into
  `REGISTERED` vs `PENDING` retries (backoff, cap → `FAILED`). The `retry_at` written at
  `SUBMITTING` doubles as the in-flight lease: a crashed writer's rows come back as orphans and
  are re-batched. `dim-tickets-writer` is **single-instance** (one inviter key = one nonce lane;
  `writer_lease` is the deploy-overlap guard) — never scale it. The internal `SUBMITTING` status
  is reported as `SUBMITTED` on the frozen wire.
- **`username-indexer` serializes projection writes on one Postgres advisory lock.** Bootstrap and
  the incremental sync loop share one lock id; a full snapshot bootstrap runs **only when there is no
  usable `sync_state` checkpoint**, and each sync pass takes the lock with `pg_try_advisory_lock`
  (skips the pass if another instance holds it). So the service is safe as N replicas on one shared
  Postgres, and restarts resume incrementally from the checkpoint (no full re-scan).
- **A checkpoint is usable only for the chain it was taken from.** `sync_state.genesis_hash` stamps
  which chain the block number belongs to; on boot, a mismatch against the connected chain means the
  whole projection restates storage that no longer exists, so the checkpoint and the rows are
  discarded together (one transaction) and a full bootstrap rebuilds them. This is automatic and
  unconditional because the projection is derived state — the alternative is an indexer that resumes
  on a position from a chain it shares no history with and serves the dead chain's usernames. A NULL
  stamp means the row predates the column and is adopted rather than rebuilt.
- **`username-indexer` public reads share one per-IP rate-limit bucket, and it is the outermost
  layer on the route.** It does no cryptography by design, so a caller that has exhausted its window
  cannot force JWT verification or puzzle hashing. The gateway forwards the GET surface to the
  indexer, while identity-owned POST/OPTIONS traffic falls through to `identity-api`.
- **The proof-of-compute gate is either-or, and off by default.** With `POC_ENABLED=false` (the
  shipping default) there is no `/api/v1/poc/issue` route and search behaves exactly as before. With
  it on, `GET /api/v1/usernames/search` admits a caller that presents **either** a valid
  identity-service JWT **or** a solved puzzle: `POST /api/v1/poc/issue` returns an HMAC-signed
  `{sessionId, timestamp, difficulty, checksum}` (no row written — issuance is stateless), and the
  client returns `Proof-Of-Compute: base64(sessionId:timestamp:difficulty:counter:checksum)` where
  `sha256(sessionId || timestamp || counter)` has at least `difficulty` leading zero bits.
  Verification order is checksum → expiry → work → **one-shot consume** (`spent_puzzles`, a
  primary-key insert, so a puzzle cannot be replayed against another replica — legacy's in-memory
  map could). A bearer token that fails verification degrades to *anonymous* and never yields a
  `401`: search must not become an authenticated route. Rejections are `400` for a malformed header
  and `402` for everything else, with the legacy detail strings. Both shipping mobile apps
  authenticate search, so the gate costs them nothing; the anonymous callers are the Desktop/host
  path (and Android's `DISABLE_AUTH` nightly build).
- **`notifications` is an isolated failure domain.** It is stateless, DB-free, and shares no
  `depends_on` with any other service, so `/readyz` needs no external probe (readiness = the bound
  listener; provider outages are per-request `200 success:false`, never unreadiness) and stopping it
  or feeding it bad provider secrets can only fail `/api/v1/notify`. Its `/api/v1/notify` route is
  rate-limited **per authenticated subject** (JWT `accountId`) keyed on `route:subject` — never raw
  client IP — enforced after JWT verification, `429 RATE_LIMITED` on exceed. It holds only the JWT
  **public** key (verify-only); push secrets (`APNS_*`/`FCM_*`) are service-owned.

## External references

- **Canonical username-registration spec** (ground-truth for the `/usernames` write path + future
  eligibility): the authoritative "New JWT / Integrity / PoUD / Username Claim Logic" spec — covers
  Android TEE attestation (`POST /auth/android/attestation`), PoUD (Android `{androidId, widevineId}`
  / iOS DeviceCheck), the INSTANT / PAYMENT_REQUIRED / QUEUED decision flow, the balance-priority
  queue (G1<10 … G4≥1000), QR-voucher bypass, and `/usernames/payment-status` +
  `/registration/queue`. Digest it fully before implementing the attestation/eligibility plan.
