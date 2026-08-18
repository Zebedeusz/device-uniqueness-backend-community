| DIM/PoI tickets | App → pallets directly; PoUD/personhood policy grants self-start instead of backend pool | Pallet flows ✅; issuance policy = runtime change | Yes | Never port daemons/pool |

> Status: research write-up (draft). Grounds the decentralization targets from Karim's
> "Device Uniqueness Backend" proposal (2026-06-22, LGTM Torsten) and the design doc's Future
> Work section in what actually exists today in three codebases:
>
> - **polkadot-bulletin-chain** (`../polkadot-bulletin-chain`, HEAD 2026-07-08) — content-addressed blob storage parachain.
> - **polkadot-sdk** (`../polkadot-sdk`, `stable-bulletin` fork) — statement store, view functions, transaction storage/bitswap, OCW.
> - **individuality** (`../individuality`, `main`) — the People/Asset Hub runtimes: people-lite, resources, game, proof-of-ink, dotns-gateway, origin-restriction.
>
> Verdict format per item: what the proposal asks, what the chain already provides,
> the candidate mechanisms (ranked), and what it removes from identity-backend-rust.

## The building blocks (what the ecosystem actually gives us)

Before the per-feature paths, the four primitives everything below composes from:

1. **Statement store** (polkadot-sdk fork: `substrate/client/statement-store/`, `substrate/primitives/statement-store/`). Off-chain, gossiped pub/sub: statements carry up to 4 topics, an optional 32-byte channel with last-write-wins per `(account, channel)`, ~1 MiB max size, priority-by-expiry, 48h purge. Clients subscribe over RPC (`statement_subscribeStatement` with topic filters — replays matching statements, then streams; spec-v2 `statement_unstable_*` adds dynamic filters). ECIES encrypt-to-account supported for private payloads. **Writes are allowance-gated in raw chain state** — and the `resources` pallet in individuality *already administers those allowances per person on-chain* (`StmtStoreSlotsPerPeriod`, `LitePersonStatementLimit`, `set_statement_store_account`, `claim_long_term_storage` — `pallets/resources/src/lib.rs:156-289,930-1062`; runtime values in `runtimes/next-people-paseo/src/people.rs:1061-1133`). Caveat: the statement-store API in this fork (#11989) differs from stock upstream — pin accordingly.

2. **Bulletin chain** (`pallets/transaction-storage/`). Feeless, authorization-gated, content-addressed blob storage: `store(data)` up to 2 MiB per tx, ~14-day retention with renewal/auto-renew, read over **IPFS Bitswap** (node `--ipfs-server`; Helia/Kubo today, smoldot `bitswap_block` on the roadmap). People Chain is already a trusted XCM authorizer (`authorize_preimage`/`authorize_account` from a sibling parachain — `runtimes/bulletin-westend/src/storage.rs:105-128`), and `resources::claim_long_term_storage` is the person-facing hook into that flow. Good for **publishing data**; provides no pub/sub, no naming, no notifications.

3. **Direct chain reads from the device** (polkadot-sdk RPC): `chainHead_v1_storage` / `archive_v1_storage` (and legacy `state_getKeysPaged`) give light clients paged storage access; FRAME **view functions** (`#[pallet::view_functions]`, queryable via `state_call`) give bounded typed lookups. The app already confirms registration on-chain via `UsernameOwnerOf` — the same read works for availability.

4. **On-chain personhood machinery** (individuality): `people-lite::attest` gated by per-attester `AttestationAllowance`; ring roots propagated People Chain → Asset Hub via `members-notifier`/`members-subscriber` XCM so **full persons can prove membership on AH without any backend**; `game` pallet's complete invite flow (`grant_invites`, `set_invite_ticket`, `sign_up_with_invite`); `origin-restriction` for per-entity fee/usage quotas; `dotns-gateway` (now on the Asset Hub runtime, #983) calling the DotNS contract via `pallet_revive`.

---

## 1. Push notifications + statement pipeline

**Proposal:** move to a dedicated notifications service, *or* push the subscription model to the client and let devices read the chain directly. Comment threads: pure client-side subscription can't deliver reliable near-realtime (OS background execution quotas — Valentin); Notifications V3 = **Avidity**, Gav says Q4 priority, not launch.

**Paths, ranked:**

- **1a. Statement-store-native fan-out with a minimal relay (recommended for launch).** Signaling can move toward the statement store; the only inherently centralized step is the APNs/FCM hop, which Apple/Google force through vendor credentials. Shape: senders eventually publish to statements (topic = recipient-derived; ECIES for private payloads); the device subscribes directly via `statement_subscribeStatement` **when running** (this is also the deGoogled-device fallback); a thin **notify-relay** — the slice already active in `docs/plans/active/notify-relay.md` — subscribes to the same topics and translates to APNs/FCM for wake-ups. With app changes in scope, make statement publication the primary path now; keep `/api/v1/notify` only as a legacy compatibility shim if old clients must remain live. The relay still needs a small push-token/endpoint registry and rotation flow; subscription preferences can later live as channel statements (last-write-wins per `(account, channel)` is a built-in preference register). Replacement relays are possible, but not automatic: clients need a relay discovery/registration model and privacy rules for which relay can observe which topics.
- **1b. Client-only chain subscription.** Works for foreground/deGoogled; rejected as the sole mechanism (OS background quotas). Keep as the fallback tier of 1a.
- **1c. Avidity / Notifications V3.** The dedicated-service path; Q4. Design 1a so the relay is disposable and Avidity replaces it without touching identity-core. Note comment [ax] from the rewrite review: v3 must cover **web push for the DIM2 SPA**.

**Removes from identity-backend-rust:** the planned notify-relay keeps shrinking (no legacy `/subscriptions` DB semantics; token registration is the remaining APNs/FCM-specific state; `/notify` ingress is optional legacy compatibility rather than the target path). The push/statement **schema endpoints** (`/schemas/*` in the legacy backend) become content published to Bulletin (CID-addressed, renewable) or DotNS-resolved — no service required.

**Gaps to close:** statement-store allowances must be granted for the accounts that publish directly from the app or relay (the `resources` machinery exists but the flow needs wiring);  the statement-store node service must actually be enabled on the target People Chain deployment (the pallet-side allowances exist in individuality, but no node config lives in that repo); relay discovery/registration, push-token rotation, and topic privacy rules; fork-vs-upstream statement API pinning.

## 2. Username indexer / search

**Proposal:** keep the indexer in scope ("unless Bastiqlite comes first"). Comments: concat hashers prevent on-chain prefix queries (Valentin); smart-contract indexing is an anti-pattern (Torsten); real decentralization needs node-level support ("Bastiqlite" — research-y, per Karim); maybe autocomplete isn't needed at all (Talles).

**What the chain confirms:** `resources::Consumers` and `UsernameOwnerOf` are plain storage maps with hashed keys — **no on-chain prefix search is possible**. `resources` has view functions today, but they expose statement-store/friend-request proof contexts, not username availability or search (`pallets/resources/src/lib.rs:1117-1136`). The comment-thread skepticism is accurate.

**Paths, ranked:**

- **2a. Exact-match only, on-device (recommended if product can drop autocomplete).** If autocomplete is dropped (Talles's question), search reduces to exact lookup = a `UsernameOwnerOf` storage read via light client. Zero backend. Because app changes are in scope, this can be part of the first release rather than a later migration.
- **2b. Published-index snapshots on Bulletin (recommended if autocomplete stays).** The indexer (or any party — the projection is deterministic from finalized state, so snapshots are verifiable/reproducible) periodically publishes a compact sorted username index (FST/trie, ~small even at millions of names) to Bulletin via chunked `store` calls plus a DAG-PB manifest. For chunked snapshots, use an account-authorized publisher; a single `authorize_preimage(root_cid)` only covers the manifest/root blob, not every chunk. Retention is ~14 days, so publish new snapshots on a cadence and renew only the currently advertised root if longer availability is needed. The app fetches the CID (over Bitswap/Helia; smoldot `bitswap_block` later) and does **on-device autocomplete**, with statement-store delta topics between snapshots if freshness matters. Search then works offline and no server is in the read path. With client work allowed now, this is a launchable architecture if product keeps prefix search.
- **2c. Keep the Rust username-indexer only as a bootstrap/snapshot publisher.** Accepted by the proposal and already shipped in this workspace, but no longer the recommended read path. It should either disappear with exact-match search or become the deterministic publisher for 2b.
- **2d. View function for bounded lookups (small runtime addition).** A `#[pallet::view_functions]` on `resources` for exact availability + owned-usernames-of-account would formalize 2a without exposing raw storage layout to clients. Cheap runtime change; worth proposing to the individuality team regardless.
- **2e. Bastiqlite / SQL-in-node.** No trace of it in any repo; treat as vapor until a spec exists. Don't plan against it.

**Removes from identity-backend-rust:** 2a/2b retire the username-indexer service (or demote it to a snapshot publisher that can crash harmlessly).

## 3. Username availability + discriminator assignment

**Proposal/rewrite-doc Future Work:** availability on-device via `UsernameOwnerOf`; discriminator/digit suffix assignment moved out of the centralized backend.

**What the chain shows:** the direction is already decided in code, but the current implementation is more precise than "chain-assigned." `dotns-gateway::reserve_name` deliberately excludes the digit suffix from the candidate-signed message, then accepts the full `lite_label` (including digits) as an unsigned extrinsic argument used for contract calldata and `LiteLabelOwner` storage (`pallets/dotns-gateway/src/lib.rs:285-385`; `pallets/dotns-gateway/src/types.rs:71-77`). The legacy `resources` username extrinsics are explicitly deprecated in favor of the DotNS path (`pallets/resources/src/lib.rs:19-27`). The rewrite-review thread ([aj]–[al]: caller-provides vs chain-assigns, "implement in the new DotNS pallet") is being answered by the dotns-gateway design, but if the product requires the chain/contract itself to choose the suffix, that is a remaining DotNS contract/runtime requirement rather than something already proven by this pallet.

**Paths:**

- **3a. On-device availability check (do now)** — light-client reads of the relevant exact keys (`UsernameOwnerOf` for legacy resources names; `LiteLabelOwner`/DotNS contract state on AH for the DotNS world). Retires `POST /usernames/available` — currently a direct-chain read in device-attestation anyway, so the backend code is already shaped for this. If the UX keeps the "base + available digits" response, the app needs either a bounded loop over the candidate suffix range or a small runtime/contract view that returns the available set.
- **3b. Track the DotNS migration in the write path.** identity-backend-rust's chain-writer currently submits `PeopleLite.attest` (+ optional DotNS reservation). As `resources` usernames deprecate, the writer's target becomes `dotns-gateway::reserve_name` on **Asset Hub** — which changes the chain the writer connects to and keeps the attester-allowance gating. Watch the "pop ↔ dotns chat" outcome ([y] in the proposal: reservation-by-attester "will change"); if the app ends up calling `register_name`/reserve directly (needs PC→AH ring-propagation latency for proof generation, per individuality#755), the backend's username write path shrinks to lite-only.

## 4. Registration itself (the "backend-less" question)

**Rewrite-doc Future Work calls this speculative. The chain makes it two very different stories:**

- **Full persons: already decentralizable today.** `dotns-gateway::register_name` verifies a caller-supplied **ring proof** against `members-subscriber` on Asset Hub (rings propagated from People Chain via XCM). No attester, no backend. The app can register a full-person username entirely on-chain once it can generate ring proofs (the propagation latency caveat is real but bounded).
- **Lite persons (device-attested): the backend remains a necessary evil, but a *bounded* one.** Apple/Google attestation verification requires vendor credentials and off-chain HTTP — a chain can't do it (comment [bc]: the attester holds the PGAS/attestation allowance). What *can* improve:
  - **Shard the attester.** `AttestationAllowance` is per-account (`people-lite::attest` decrements the verifier allowance before registering the lite person), so multiple independent attesters (different operators, each with its own allowance and proxy) turn the single trusted backend into a federation. Also the writer-scaling answer (proxy-per-attester = independent nonce lanes).
  - **Quota-bound the attester on-chain.** `origin-restriction` + allowances already cap what a rogue/compromised attester can do — lean on that in the security story ("the backend can only spend its allowance").
  - **OCW-oracle variant** (chain emits registration-request event → off-chain worker verifies with vendor APIs → submits attest): the pattern exists in-SDK (`pallet-statement`'s OCW is the reference), but it just moves the vendor credentials into node operators' keystores — a custom node extension with a worse trust story than a sharded attester. Not recommended.
  - The **"quotas & queries"** concept (Gav/Mike retreat doc, required for launch per the proposal thread) has **no implementation in individuality yet** — nearest primitives are `origin-restriction` + `AttestationAllowance`. Flag this as unscoped work living with the runtime team, not the backend.

**Removes from identity-backend-rust:** nothing at launch; long-term the registration eligibility/queue slice stays the true core, and everything else in this document peels away around it — which is exactly the proposal's "identity does attestation and usernames, nothing else."

## 5. Invites

**Proposal:** revisit — adjacent to identity but coupled to community mechanics. Rewrite-doc Future Work: on-chain invite graph.

**What the chain shows: invite distribution and sign-up are on-chain already, but issuance is still privileged.** `game::grant_invites` (privileged `InviteIssuer` origin) → `set_invite_ticket` (holder distributes; a ticket is an account whose key is revealed to the invitee — the same custodial-key shape the backend pool uses) → `sign_up_with_invite` (invitee proves ticket possession; free while `score > 0`).

**Paths:**

- **5a. Retire the pool via issuer grants (no runtime change, transitional).** Instead of the backend pre-minting a 1,000,000-key custodial pool (the current TS-side design), the `InviteIssuer` grants invite budgets to persons directly; the app calls `set_invite_ticket`/`sign_up_with_invite` itself. This decentralizes distribution and removes pool custody from identity-backend, but issuance policy is still centralized in the privileged `InviteIssuer` origin. The invite-faucet compatibility service then only bridges old clients and dies with them.
- **5b. Personhood-granted invites (small runtime change).** "Each personhood grants N invites" needs `game` to auto-grant budgets to persons (today only the privileged issuer can) — a small pallet change plus anti-abuse thought (score-gating exists). This is the real "invite graph" and it eliminates pool-exhaustion (the Summit failure) structurally.

**Removes from identity-backend-rust:** the invite-faucet slice becomes transitional-only; don't invest beyond compatibility.

## 6. DIM / game / ProofOfInk tickets

**Proposal:** ProofOfInk can stay identity-adjacent; game tickets follow the decentralize path (and comment [m]: same purpose, same path for both).

**What exists today is two backend bridges over the same pallet primitives, not identity logic:**

- `/api/v1/dim-ticket` queues the user's own account as the ticket, checks the backend inviter's remaining `AvailableInvites`, and a daemon later calls `Game.set_invite_ticket` or `ProofOfInk.set_invite_ticket` on-chain. This avoids storing a ticket private key, but still centralizes registration through the backend daemon and the backend inviter allowance.
- `/api/v1/invitation-ticket/claim` keeps an already-registered pool of disposable ticket keypairs. The backend stores the private key, marks one ticket claimed, signs the user's account with that ticket key, and returns `(ticket, inviter, signature)`. This is the Summit-style faucet/pool and the part we should avoid porting.

**What the chain already provides:** both DIMs have the actual execution flow on-chain. `game` has `grant_invites` (privileged issuer) → `set_invite_ticket` (invite holder creates pending ticket) → `sign_up_with_invite` (ticket signs the invitee account). `proof-of-ink` mirrors that shape with `AvailableInvites`, `PendingInvites`, `set_invite_ticket`, and `apply_with_invitation`; the `AsProofOfInkParticipant` extension validates the ticket signature and rewrites the origin to `InvitedCandidate`. Both pallets expose small runtime APIs for deposit queries (`PalletGameApi::play_deposit`, `ProofOfInkApi::candidacy_deposit`), so the app does not need a backend to calculate the normal fee/deposit path either.

**Recommended path with client changes allowed:** do not port either backend daemon to identity-backend-rust. The app should submit pallet calls directly and query the runtime APIs/light-client state directly. For already-funded or already-person accounts, use the normal pallet paths. For fresh-device onboarding, replace the pool with a policy primitive: PoUD/lite-attester/personhood grants a bounded invite allowance or a one-shot self-start right, then the app calls `set_invite_ticket`/`sign_up_with_invite` or `apply_with_invitation` itself. Best UX is a new atomic "claim device invite + join/apply" runtime path; the minimal runtime change is "grant `AvailableInvites(account) = 1` after PoUD" and let the app spend it.

**Runtime/product decisions still needed:** issuance is still privileged today (`InviteIssuer`/`InvitationsOrigin = EnsureRoot` in the People runtime), so removing the pool requires a pallet policy change. Do not give every unique device broad outgoing invite power; that is a Sybil multiplier. Safer launch policy: one PoUD grants only self-use, while transferable invites unlock through personhood, score/progress, cooldown, or governance.

## 7. TURN

**Proposal:** standalone coturn with client-minted shared-secret creds, or a tiny TURN-broker. Comments: client-minting needs the secret on the device (bad), integrity-gating should extend to any Triangle app, limited coturn auth options.

**What the repos show:** nothing chain-side helps here — no relevant primitive in any of the three repos (signaling is already decentralized via the statement store; the media relay is the residual centralized piece).

**Paths, ranked:**

- **7a. Thin JWT-gated `/turn/issue` broker (V1, as planned).** coturn REST-API TTL credentials; secret stays server-side; verify-only JWT via `auth-verify`. Matches the rewrite doc.
- **7b. Personhood-gated broker (incremental).** Same broker, but accept a ring-proof or People-Chain-verifiable credential instead of (or alongside) the identity-core JWT — answers the "any Triangle app, not just mobile" integrity question without embedding secrets. Worth a short design note with Novasama.
- **7c. Relay decentralization (research, Future Work).** Mesh/self-hosted relay capability — keep on the research track from the rewrite doc; no current substrate to build on.

## 8. Attestation evidence, schemas, and other publishable artifacts

Cross-cutting: anything the backend serves as **static content** — statement/push-payload JSON schemas, the attester public key, index snapshots (2b), possibly public attestation-evidence commitments — fits Bulletin's model exactly: feeless authorized `store`, CID addressing, auto-renew, IPFS reads, People-Chain XCM authorization already wired, `resources::claim_long_term_storage` as the person-facing entry. DotNS gives the stable name → CID indirection. Each artifact moved this way deletes an endpoint from the retirement list rather than porting it.

---

## Summary table

| Proposal item | Mechanism | Exists today? | App work now? | Effect on identity-backend-rust |
|---|---|---|---|---|
| Push fan-out | App publishes statements + minimal APNs/FCM relay (1a) | Statement store ✅, allowances ✅ (wiring needed) | Yes | notify-relay shrinks to push-token/credential holder; `/notify` becomes optional legacy shim |
| Statement pipeline | Native statement store, allowances via `resources` | ✅ | Yes | Nothing to port (already dropped) |
| Schemas endpoints | Bulletin `store` + CID (+ DotNS name) | ✅ | Yes | Endpoints deleted, not ported |
| Username search | Exact-match on-device (2a) or Bulletin snapshot + on-device index (2b) | Storage ✅; snapshot format = new | Yes | Indexer → snapshot publisher, or retired |
| Availability | Light-client `UsernameOwnerOf` read (3a) / view function (2d) | Storage ✅; view fn = small runtime PR | Yes | `/usernames/available` retired now |
| Discriminator | DotNS lite suffix excluded from candidate signature; chain/contract assignment still needs confirmation | Partial | No app blocker; runtime/contract decision | Writer retargets to AH dotns-gateway |
| Registration (full person) | `register_name` + self-generated ring proof | ✅ | Yes (proof gen) | Full-person path leaves backend |
| Registration (lite) | Sharded, allowance-bounded attesters | Allowances ✅; sharding = ops | No app blocker; attester ops | Stays core — by design |
| Invites | `grant_invites`/`set_invite_ticket`/`sign_up_with_invite` (5a→5b) | ✅ (issuer-origin change for 5b) | Yes | Faucet transitional-only |
| DIM/PoI tickets | App → pallets directly; PoUD/personhood policy grants self-start instead of backend pool | Pallet flows ✅; issuance policy = runtime change | Yes | Never port daemons/pool |
| TURN creds | Thin broker (7a), personhood-gated when proof format is chosen (7b) | Nothing chain-side | Yes for 7b | Stays thin service |
| Anti-Sybil quotas | "quotas & queries" — unimplemented; nearest: `origin-restriction` | ❌ | — | Runtime-team dependency, flag it |

## Suggested sequencing

1. **Now (client changes allowed):** make the app publish/read statements directly where possible, with notify-relay reduced to APNs/FCM wake-ups plus token registration; keep `/api/v1/notify` only as a legacy shim if needed. Move availability to on-device exact reads, choose exact-match search or Bulletin-backed autocomplete, route game/PoI through pallet calls/runtime APIs, and publish schemas/attester-key artifacts to Bulletin. Do not port DIM-ticket, subscription, schema, or username-availability surfaces.
2. **Now, in parallel with runtime/contract owners:** propose the `resources` view function (2d), decide whether DotNS must truly assign suffixes on-chain/contract-side, scope personhood-granted invites (5b), and scope the PoUD/self-use DIM invite grant (6). These are not blocked by app release timing; they are runtime/product decisions.
3. **After external dependencies land:** track Avidity for relay replacement, smoldot `bitswap_block` for better decentralized Bulletin reads, and any DotNS reservation changes that affect the writer. Personhood-gated TURN (7b) can ship whenever the app/broker proof format is chosen.
4. **Standing dependencies to watch:** "quotas & queries" (runtime team, required for launch per proposal thread), smoldot `bitswap_block` (decentralized Bulletin reads), statement-store node enablement + allowance granting on the production People Chain, PC→AH ring-propagation latency for app-side proofs.
