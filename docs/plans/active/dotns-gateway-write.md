> Stage: active. Keep this plan current while working. When a scope item is finished, check its box `[ ]`→`[x]`. When a phase's ✅ demo passes, mark the phase done, with date. The plan file is the source of truth, not memory.

# Context

Since paritytech/individuality#755 there are **two independent username stores on two chains, with no bridge between them**:

1. **People chain `Resources::UsernameOwnerOf`** — the deprecated pre-#755 home. `PeopleLite.attest` → `register_lite_consumer` → `Resources` (`individuality/runtimes/next-people-paseo/src/people.rs:1052` binds `LiteConsumerRegistrar = Resources`). This is all `identity-chain-writer` does today (`crates/identity-service/src/chain/writer.rs:646`).
2. **Asset Hub `dotns-gateway` + the dotNS contracts** — what clients actually read. Written **only** by the `reserve_name` extrinsic (`individuality/pallets/dotns-gateway/src/lib.rs:317`).

The only People→AH XCM in individuality is ring-root propagation, not usernames. So a lite username registered through this backend exists on People and **does not exist in dotNS**. The legacy TypeScript backend closed the gap with a separate dotns daemon (`pop-dotns-testing/src/reserveNameBackend.ts`: *"Without this block the inserted row stays ahStatus='PENDING' and the dotns daemon never submits"*). This repo has no equivalent. `grep -c dotns crates/identity-service/src/chain/writer.rs` is 0. The `Reservation` struct the writer selects (`crates/identity-service/src/chain/outbox.rs:260`) does not even carry the dotns columns.

Everything `reserve_name` needs is **already in Postgres**, collected at intake and never used:

| `reserve_name` arg | `username_reservations` column |
|---|---|
| `candidate` | `candidate_account_id` |
| `candidate_signature` | `dotns_signature` |
| `lite_label` | `full_username` (`base.digits`) |
| `chat_key` | `identifier_key` |
| `reserved_base_label` | `reserved_username` |
| `signed_at` | `dotns_signed_at` |

`reserve_name` needs no prior People attestation. It is `ensure_signed(attester)` + allowance + the candidate's signature + a freshness window. It then writes `LiteLabelOwner` and calls the contract. The candidate signature deliberately commits to the **base only**, so the backend's digit allocation stays valid.

**The two Asset Hubs run different `reserve_name` extrinsics** (measured 2026-08-06). Only PreviewNet carries the signature mechanism this backend already collects for:

| | PreviewNet AH `2000034` | Paseo next AH `2000033` |
|---|---|---|
| args | `candidate, candidate_signature, lite_label, chat_key, reserved_base_label, **signed_at**` | `candidate, candidate_signature, **ring_vrf_key, proof_of_ownership**, lite_label, chat_key, reserved_base_label` |
| constants | `MaxValiditySeconds=259200`, `MaxFutureSkewSeconds=30` | *(neither)* |
| errors | `ReservationSignatureExpired`, `ReservationSignatureFromFuture` | `InvalidProofOfOwnership` |

PreviewNet is the mechanism `individuality` `main` implements (`pallets/dotns-gateway/src/lib.rs:317`). It is also the one both reference implementations target. Paseo next AH still runs the older proof-of-ownership shape, which has no `signed_at` at all. The stored payload does not fit it.

**Outcome**: a registered lite username appears in the dotNS store clients read, on every environment whose Asset Hub runs the PreviewNet-shaped gateway.

# Goals

- `identity-chain-writer` submits `DotnsGateway::reserve_name` on Asset Hub for every registration that carries a `dotns` block and has landed on People. It submits the **PreviewNet-shaped, `signed_at` variant**.
- The local run against the deployed PreviewNet chains (`.local/previewnet.env`) ends with the label resolvable through the dotNS contracts. Today that run reaches *"registration assigned on-chain"* and then has no AH record.
- The deployed `identity-previewnet.dotspark.app` environment does the same end to end.
- Pointed at an Asset Hub whose `reserve_name` is a different shape, the writer **parks the dotNS lane** rather than mis-encoding a call. It keeps attesting on People.
- A reservation that cannot be written to AH (bad signature, expired signature, label already claimed) reaches a **distinct, observable terminal state** without touching the People registration, which stays valid.
- One image, one process, one writer instance. No second binary, no second lease, no second codegen.

# Non-goals

- **Not enabling paseo-next-v2** (`identity.dotspark.app`), and **not** implementing the old proof-of-ownership `reserve_name`. Its Asset Hub runs a different extrinsic. Supporting it means a second encoding we would delete on the next Paseo AH runtime upgrade. Blocked on that upgrade, not on our config — see Later.
- **No full-person registration.** `register_name` + the `AsDotnsGateway` extension + ring proofs are a different extrinsic with a different origin. Out of scope.
- **No dotNS reads.** `username-indexer` keeps projecting People only. Nothing queries the contracts back into the search index.
- **No re-signing.** Only the client holds the candidate key. The backend can never refresh an expired reservation signature.
- **No backfill.** Rows created before this slice are never submitted to Asset Hub.
- **No second chain in `identity-api`.** Intake shape-checks the block; it never opens an AH connection.
- No new deployable service and no `docker-compose.yml` host ports.

# Design principles

- User journey drives order. The local previewnet run must go green before anything is deployed.
- Reuse before rebuild. Same writer process, same `writer_lease`, same `chain_client::connect`, same extension tuple, same dynamic-call pattern, same `*_live.rs` test shape.
- Chain is the source of truth for chain constants. The validity window comes from AH metadata, not from an env var that can drift.
- Fail at the earliest boundary that can tell **and that owns the failure**. Malformed block at intake; bad signature, expiry and future-dating at the writer, before an extrinsic is spent. A dotNS problem never costs the caller its People username.
- Two chains, two failure domains, two statuses. Never one status that means both.

# User journey

1. A client registers a username and includes the optional `dotns` block (signature, `signedAt`, optional `reservedUsername`), bound to whatever `GET /api/v1/attester` returned.
2. `identity-api` validates the block's shape and freshness bounds and returns `202`. It does **not** verify the reservation signature — see Phase 5.
3. `identity-chain-writer` lands `PeopleLite.attest` on People. The row reaches `ASSIGNED`.
4. The same writer pass submits `DotnsGateway::reserve_name` on Asset Hub. The row reaches `dotns_status = RESERVED`.
5. The label resolves through the dotNS contracts. The candidate holds the lite NFT, the chat key is persisted, and any `reservedBaseLabel` is parked for a later full-person claim.

# Approved architecture (decided — do not relitigate while building)

Every on-chain fact below was measured **2026-08-06** against the live networks.

## Two nonce lanes, one process, one lease

A lane is **(signing account, chain)**. Adding Asset Hub adds a second lane on the *same* account. That is safe for the same reason the two environments already share one account across two chains: **nonces are per chain**. What must not happen is two *submitters* on one lane.

The guarantee is kept structurally, not by convention:

- The AH lane lives **inside the existing `identity-chain-writer` process**, in the same sequential row loop, behind the same `writer_lease` row (`people-chain-writer`). The lease is per process, not per chain. One lease therefore still means one submitter on both lanes.
- `identity-chain-writer` stays `deploy: replicas: 1` and stays on the "don't scale" list in `AGENTS.md`.
- A second cached nonce (`next_nonce_ah`) sits beside `next_nonce`, reset to a chain re-fetch on any error. That is the identical discipline `writer.rs` already applies.
- **No second process and no second lease name.** A separate dotns worker would need its own lease. Nothing would order its submit against the People `ASSIGNED` transition it depends on.

## Sequential, People first

`reserve_name` needs no People attestation and `attest` needs no AH record. The write order is still **not** free. `reserve_name` inserts `LiteLabelOwner[lite_label]`, a **global claim on the label**. Claiming it for a username whose People `attest` failed would squat a name nobody owns. AH is therefore attempted only for rows already `ASSIGNED` on People. It runs as a second pass in the same row loop, and as a standalone claim query (`status='ASSIGNED' AND dotns_status IN ('PENDING','RETRY_AFTER')`). A row whose AH submit failed is then retried on a later scan without re-touching People.

## Two statuses on one row

`status` (People) and `dotns_status` (Asset Hub) advance independently. **A dotNS failure never changes `status`.** `ASSIGNED` + `DOTNS_FAILED_TERMINAL` is a legitimate resting state: the username works, the dotNS name does not. The dependency runs one way only: a People `FAILED_TERMINAL` closes an open dotNS half as `ABANDONED` in the same guarded statement, because `claim_dotns_due` requires `ASSIGNED` and such a row would otherwise sit in `PENDING` forever, inflating the lane's depth gauges.

```
dotns_status:  NULL ── no dotns block on the request (or a row predating this slice)
               PENDING ──People ASSIGNED, gates pass──▶ SUBMITTING ──finalized──▶ RESERVED
               SUBMITTING ──transient──▶ RETRY_AFTER ──not_before──▶ (re-picked)
               PENDING/SUBMITTING ──bad sig / label owned by another / unbuildable──▶ FAILED_TERMINAL
               PENDING ──signed_at + MaxValiditySeconds < now──▶ EXPIRED
```

`NULL` deliberately means two things: "no dotns block" and "created before this slice". Chosen over a backfill so the **first writer boot after deploy fires zero Asset Hub extrinsics**. `created_at` separates the two if it ever matters, and the column comment says so. An operator can hand-flip a specific recent row to `PENDING`.

## One target call shape, asserted at boot

The writer targets the **PreviewNet shape only**: `(candidate, candidate_signature, lite_label, chat_key, reserved_base_label, signed_at)`. It never tries to detect the runtime and switch encodings.

The two live Asset Hubs genuinely disagree on this call. "Which chain am I pointed at" therefore cannot be left to configuration discipline. At startup the writer reads the connected AH's metadata for `DotnsGateway::reserve_name`. It compares the **field names, in order**, against that expected list, and **aborts the process** on any mismatch. Pointed at Paseo next AH it dies with `reserve_name shape mismatch: got [candidate, candidate_signature, ring_vrf_key, proof_of_ownership, …]` instead of encoding a positionally-wrong call. The metadata is already loaded at connect, so this costs a few lines. It removes an entire class of silent wrong-chain failure — the same instinct as `previewnet-env.md`'s "a wrong hostname simply fails".

## Dynamic call by name — no second codegen

`reserve_name` follows the existing pattern: `writer.rs:630` builds `PeopleLite.attest` / `Proxy.proxy` via `subxt::dynamic`. The AH reads — `DotnsGateway::LiteLabelOwner`, `AttestationAllowance`, and the two validity constants — use `subxt::dynamic::storage` and metadata constants.

Justification, since `chain-types` exists precisely to hold typed chain surface. Codegen pays for itself when a typed surface is shared across crates and exercised widely. That is exactly the People case: three crates, storage + calls + constants. Asset Hub would buy type-checking on six arguments — a 32-byte account, two byte blobs, a length-capped label, an `Option<label>` and a `u64`. The cost is vendoring ~500 KB of the **fastest-churning runtime in the family** (revive, XCM, assets) into the repo, plus a second `#[subxt::subxt]` module that every `cargo build` expands.

The call-shape divergence *strengthens* this rather than weakening it. Vendored codegen pins one runtime's shape at compile time and gives a confusing type error against the other. The boot assertion above gives a named, actionable runtime error against **whatever the operator actually connected to**, including a future shape neither metadata blob knew about. The registration call this writer already submits is dynamic for the same reason. `previewnet-env.md` records that choice as the thing that made one binary serve two People runtimes.

## One extension tuple per chain family

> **Amended while building (2026-08-06).** This section originally said *one tuple, four
> runtimes*. subxt's `TransactionExtensions` tuple impls stop at **26 members**
> (`subxt-0.50.2/src/config/transaction_extension_traits.rs`) and the merged People+Asset Hub
> set is **28**. The merge is therefore not expressible. `chain-types` now carries a second
> config, `AssetHubConfig`, with its own 18-member tuple. Both still live only in `chain-types`,
> so the shipped "`chain-types` is the only place chain types live" contract holds. The accepted
> cost is two tuples to keep in step. The accepted benefit is that the proven People pipeline is
> untouched by this slice. Everything below about *which names* Asset Hub declares is unchanged.

Measured extension lists:

| Chain | spec | tx | slot 0 |
|---|---|---|---|
| PreviewNet AH | `next-asset-hub-paseo` 2000034 | 18 | `UnitTransactionExtension<Null>` |
| Paseo next AH | `next-asset-hub-paseo` 2000033 | 18 | `AuthorizeValueTransfer<Option<[u8;64]>>` |

Both then declare, identically:
`AuthorizeCall, AsPgas, AsRingAlias, AsDotnsGateway, RestrictOrigins, CheckNonZeroSender, CheckSpecVersion, CheckTxVersion, CheckGenesis, CheckMortality, CheckNonce, CheckWeight, ChargeAssetTxPayment, CheckMetadataHash, EthSetOrigin, StorageWeightReclaim`

The slot-0 split is **the same one `chain-types` already carries for People**. No new mechanism. Note the contrast with the call shape: the *signing pipeline* on the two Asset Hubs is identical modulo slot 0, while the *extrinsic* is not. Paseo next AH stays in the snapshot test anyway. The tuple must remain able to sign there, and the snapshot costs nothing. Relative to the People tuple, `AssetHubTransactionExtensions` drops `VerifyMultiSignature` and the People-only gates, and adds five names:

- `AsPgas`, `AsRingAlias`, `AsDotnsGateway` — all `Option<…>`, so plain `Noop` with `VALUE = &[0]`, exactly like `AsPerson` and friends.
- `EthSetOrigin` — `Null`, so `Noop` with an empty value.
- `CheckMetadataHash` — **must be subxt's real `transaction_extensions::CheckMetadataHash`, not a `Noop`**. Its value is `{mode}`, 1 byte, but its *implicit* is `Option<[u8;32]>` = `None`. `Noop::encode_implicit_to` writes nothing, which is a signer-payload mismatch and an on-chain signature failure. subxt ships it with `Params = ()` and disabled mode, so it costs one tuple slot and one `()` in the builder. (`previewnet-env.md` flagged this as the thing DevNet would need. Asset Hub needs it too.)

Two non-obvious compatibilities, both verified in source:

- `RestrictOrigins` is `bool` on AH vs `Option<…>` on People. Both encode `0x00`, so the existing `&[0]` `Noop` serves both.
- AH declares **no** `VerifySignature`. Harmless twice over. subxt resolves extensions by name against the live list, so an undeclared member is never encoded. And `subxt-0.50.2/src/transactions.rs:238` (`default_transaction_version`) prefers **v4-signed whenever the chain advertises v4**, which every runtime in this family does. The signature therefore travels in the classic prefix, and `VerifyMultiSignature` is not load-bearing on either family.

`PeopleConfig` keeps its name. A workspace-wide rename touches every service to describe the same Polkadot-shaped config. The module doc gets a sentence instead.

## Attester identity: one account, both chains

`ATTESTER_ACCOUNT` names the attester authority on **both** chains, and the writer *derives* proxying from it, so it equals the extrinsic's signed origin by construction. Clients bind `consumerRegistrationSignature` and `dotns.signature` to whatever `GET /api/v1/attester` returns. Under `Proxy.proxy(real = P)`, `ensure_signed` yields `P`. The `attester` the pallet sees is therefore the proxied primary, the same account `PeopleLite.attest` treats as verifier. **The existing config carries over with no per-chain variant.** Verified:

- **previewnet** (local `.local/previewnet.env` and the deployed env): `5FyPZEEoKDgHjX48o9vimuKRaq7ggsPBFuWxnqdEYRNttms7` (`0xace319e7…`) signs directly, no proxy. People `PeopleLite::AttestationAllowance` = 1000. AH `DotnsGateway::AttestationAllowance` = **1,000,000**. Funded on both: AH free 5.0 units, nonce 0.
- **paseo-next-v2**: signer `5HEhiB3hMb8P36L6Edj716fR4wLPUSqoZZpcUrrYEod7f3qu` (`0xe4cd20d6d1e0e119d63a943afd2d7496fbbb0ac8e7cd99c3c5f16b63a68e7432`) is an `Any`/delay-0 proxy of the shared authority `5F7H1LkZ…` on **both** People and Asset Hub. The AH `Proxy.Proxies` entry renders as `16AzrWJmDNPrUdLcCGn79FVZvZL3AkPwe4Z6e9qtntedqeou`, the same public key under SS58 prefix 0. `5F7H1LkZ…` holds AH `DotnsGateway::AttestationAllowance` = **999,125**. The signer is funded on AH, nonce 428. `CHAIN_WRITER_PROXY_FOR` therefore needs no change when that env is eventually enabled.

**What breaks if an operator configures them apart** is silent and expensive. Every `reserve_name` fails `InvalidAttestationSignature`. The signature check runs *before* the allowance decrement (`lib.rs:352` vs `:358`), so no allowance is burned. The fee is burned, because `Pays::No` only applies to the success path. The row then retries to `CHAIN_WRITER_MAX_ATTEMPTS` before dying. Guard: since #32 there is nothing to configure apart. `ATTESTER_ACCOUNT` is the single name and `proxy_target(attester, signer)` derives the proxy target from it, so a deployment cannot claim to attest as one account while signing as another. That invariant was already load-bearing for People; the dotNS lane inherits it.

## Expiry: chain-sourced, terminal, never retried

Chain constants, read from PreviewNet AH metadata at connect and logged: `MaxValiditySeconds = 259200` (3 days), `MaxFutureSkewSeconds = 30`. **Both are enforced** before an extrinsic is spent: aged out → `EXPIRED` (terminal), future-dated → deferred behind `dotns_not_before` without spending an attempt. identity-api's `DOTNS_MAX_FUTURE_SKEW_SECS` is a separate value on a separate process, so it cannot be the only guard on the skew bound. Paseo next AH declares neither constant, because its `reserve_name` has no freshness window; the shape assertion means the writer never reaches this read on that runtime, so the read can be hard rather than optional.

- **Intake is the real guard.** `DOTNS_INTAKE_FRESHNESS_MAX_AGE_SECS = 600` means a row arrives with ~3 days of writer headroom, against a drain measured in seconds. A row can only expire if it was *parked*: writer down for days, or `QUEUED` behind the registration queue.
- **The writer checks locally before every submit** (`now > signed_at + MaxValiditySeconds`) and marks `DOTNS_EXPIRED`. A distinct terminal state. Never a retry, never a wasted extrinsic, never a wasted allowance.
- **`DOTNS_EXPIRED` is a fact to expose, not an error to recover from.** The backend cannot re-sign; only the client holds the candidate key. It gets a metric and a `warn!`. A client-facing re-claim path is deferred until that metric is ever non-zero (see Later).

One default changes with it: **`DOTNS_MAX_FUTURE_SKEW_SECS` 600 → 30**, to match the chain. At 600, a signature up to 570 s in the chain's future passes intake and fails `ReservationSignatureFromFuture` on chain. The existing retry ladder (`2^attempt` capped at 64 s, 8 attempts ≈ 254 s) can exhaust *before the clock catches up*, converting a perfectly valid reservation into a terminal failure. The golden fixtures pin `Config::test_default()` at 600 (`crates/identity-service/src/config.rs:277`), so `register-400-dotns-future` still replays byte-identically.

# Foundations / Already shipped (✅)

## Single-writer outbox + lease

- What exists: the `RESERVED → SUBMITTING → ASSIGNED/RETRY_AFTER/FAILED_TERMINAL` machine in `crates/identity-service/src/chain/outbox.rs`. The `LEASE_HELD` guard on every writer mutation. The sequential row loop, cached-nonce reset, mid-submit lease renewal, and reconcile-don't-resubmit (`writer.rs:355-497`).
- ✅ Demo: a registration reaches `ASSIGNED` with a tx hash on either environment.
- Gaps: none for People. The dotNS lane reuses all of it verbatim.

## dotNS intake

- What exists: `POST /api/v1/usernames` accepts and persists the `dotns` block. `DOTNS_GATEWAY_ENABLED` gating, freshness bounds, `reservedUsername` pattern (`register.rs:838-1008`), stored in `username_reservations` (`migrations/0002_usernames.sql:25-28`). Golden fixtures cover the enabled/disabled/future/stale cases.
- ✅ Demo: `cargo test -p identity-service --test legacy_wire`.
- Gaps (pre-slice): the signature is **never verified**, only its hex shape. Nothing reads the columns back. This slice adds verification in the writer, not in intake.

## Cross-runtime signing

- What exists: `PeopleTransactionExtensions` + `PeopleExtrinsicParamsBuilder` in `crates/chain-types/src/lib.rs:73`, resolved by name against the live metadata, with the `KNOWN_RUNTIMES` snapshot test. `chain_client::connect` is already product-agnostic transport.
- ✅ Demo: `cargo test -p chain-types covers_every_known_runtime_extension_set`.
- Gaps: five extension names the Asset Hubs declare are absent from the tuple (see Approved architecture).

## Reference implementations of `reserve_name`

- What exists: `truapi/rust/crates/truapi-server/src/host_logic/dotns_gateway.rs` (`build_reservation_message`, with golden vectors) and `pop-dotns-testing/src/reservation.ts` + `src/flow/reserve.ts` (submission plus the full contract-side assertion suite).
- ✅ Demo: `pnpm reserve <base> <digits> --network previewnet` in `pop-dotns-testing`.
- Gaps: neither is reachable from this workspace. The message builder is re-implemented (Phase 2) and pinned to their golden bytes.

# Phase 0 — external prerequisite: **none**

Not a coding phase. Recorded so nobody waits on an ask. Both target environments are already provisioned. Allowance, proxy delegation and fee balance were all verified on Asset Hub 2026-08-06; the numbers are under *Attester identity*. The only environment needing an operator conversation is paseo-next-v2, which is out of scope.

# MVP phases (ship-shaped, demoable)

## Phase 1: one tuple, both Asset Hubs

- **Goal**: the existing binaries can sign on Asset Hub, with no second codegen and no second image.
- **Scope checklist**:
  - [x] **Corrected while building — one tuple was not possible.** subxt's `TransactionExtensions` tuple impls stop at **26 members** (`subxt-0.50.2/src/config/transaction_extension_traits.rs`) and the merged People+AH set is **28**. Added a separate `AssetHubConfig` + `AssetHubTransactionExtensions` (18 members) beside `PeopleConfig` instead. That also leaves the proven People pipeline byte-for-byte untouched. Both still live only in `chain-types`, so the "one home" contract holds. The "one tuple" wording does not. New `Noop`s: `AsPgas`, `AsRingAlias`, `AsDotnsGateway` (`&[0]`), and `EthSetOrigin` (empty). Plus subxt's real `CheckMetadataHash`.
  - [x] `KNOWN_RUNTIMES` gained a `tuple` field and the two Asset Hub captures. `covers_every_known_runtime_extension_set` now checks each runtime against its own family's tuple.
  - [x] `chain-client` gained `connect_asset_hub` and a second `Signer<AssetHubConfig>` impl for `WriterSigner`. Same account, two chains, two nonce lanes.
  - [x] Updated the `chain-types` module doc. Kept the `PeopleConfig` name.
  - [x] `just check` green.
- **✅ Demo — NOT YET RUN.** A throwaway binary **outside the repo**, built against `chain-types` + `chain-client`, signs a `System::remark` as the previewnet attester and drives it to **finalized on `wss://previewnet.substrate.dev/asset-hub`**. A mis-encoded extension or a missing implicit is a signature mismatch on chain, so finalization is the proof. The same technique `previewnet-env.md` Phase 1 used. No secret and no repo dependency involved.
- **Risks / failure modes**:
  - Paseo next AH is **not** covered by the demo, because its signer secret lives on the server. Its extension list differs from previewnet AH in slot 0 only, the exact split already proven on People, and the snapshot test holds it there. Signing on it is not a goal of this slice anyway: its `reserve_name` is a different extrinsic.
  - `ChargeAssetTxPayment` encodes `Option<AssetId>` as `None` against AH's `Option<StagingXcmV5Location>`. Variant-name encoding should carry it. The remark is what proves it.

## Phase 2: the reservation message, verified offline

- **Goal**: the backend can decide, with no chain, whether a stored `dotns_signature` will be accepted.
- **Scope checklist**:
  - [x] New `crates/identity-service/src/dotns.rs`: `reservation_message(candidate, attester, base, chat_key, reserved, signed_at) -> Vec<u8>`, encoding SCALE `(Vec<u8> "pop:dotns-gateway:reserve", [u8;32], [u8;32], Vec<u8> base, Vec<u8> chat_key, Option<Vec<u8>> reserved, u64)`. Plus `verify_reservation_signature(...)` over sr25519. Mirrors `Pallet::construct_reservation_message` (`dotns-gateway/src/lib.rs:344`), `build_reservation_message` (truapi `host_logic/dotns_gateway.rs`) and `buildReservationMessage` (`pop-dotns-testing/src/reservation.ts`). All three describe the **PreviewNet** mechanism. The older proof-of-ownership variant is not implemented.
  - [x] `check_reserve_name_shape(fields) -> Result<…>` lives here too. A pure function over the metadata field names, so the boot guard is testable without a chain.
  - [x] The base is `BaseLabel::lite_base()`, the bytes before the first `.`. Never the full label.
  - [x] Golden-vector test from a captured `pop-dotns-testing` run: exact message bytes **and** a passing verify. Both `None` and `Some(reserved)` shapes. The Rust encoder matched the TypeScript bytes on the first run.
  - [x] Negative cases: wrong attester, full label instead of base, `reserved` `Some`↔`None` mismatch, tampered `signed_at`, non-64-byte signature.
- **✅ Demo passed 2026-08-06**: `cargo test -p identity-service dotns` green, 6 tests. The golden vector's hex is identical to the bytes `pop-dotns-testing`'s TypeScript encoder produces for the same inputs.
- **Risks / failure modes**:
  - `Vec<u8>` vs fixed-size encoding for the two 32-byte accounts (raw, no length prefix) and the 65-byte chat key (length-prefixed). The exact trap the golden vector exists to catch.

## Phase 3: the AH write, on the local previewnet run

- **Goal**: the run that today ends at *"registration assigned on-chain"* ends with the label in dotNS.
- **Scope checklist**:
  - [x] `crates/identity-service/src/chain/asset_hub.rs` — `AssetHubClient` beside `ChainClient`, reusing `chain_client::connect` verbatim. Surface: `online()`, `health()`, `lite_label_owner(label)`, `attestation_allowance(account)`, `validity_window()` with both constants from metadata. All dynamic.
  - [x] Migration `crates/identity-service/migrations/0008_dotns_outbox.sql`: `dotns_status`, `dotns_tx_hash`, `dotns_attempt`, `dotns_not_before`, `dotns_last_error`, plus an index on `(dotns_status, dotns_not_before)`. **No backfill.** The column comment records that `NULL` covers both "no dotns block" and "predates this slice".
  - [x] `outbox.rs`: add `dotns_signature` / `dotns_signed_at` to `Reservation` and `SELECT_COLS`. `insert_with` sets `dotns_status='PENDING'` when the request carried a dotns block. Add `claim_dotns_due` and `mark_dotns_submitting/reserved/retry/failed/expired`, all behind the existing `LEASE_HELD` clause. Extend `depth_by_status`'s sibling for the dotns column.
  - [x] `writer.rs`: `build_reserve_name_tx(r, candidate, proxy_for)` mirroring `build_registration_tx`, either direct or wrapped in `Proxy.proxy(real, None, DotnsGateway::reserve_name{…})`. Plus `next_nonce_ah`, a dotNS pass after the People pass in the same row loop, and the standalone `claim_dotns_due` scan.
  - [x] Pre-submit gates, cheapest first. Each one avoids an extrinsic:
    1. attester match (asserted once at startup, see below);
    2. `verify_reservation_signature` (Phase 2) → `FAILED_TERMINAL`;
    3. expiry against the chain constant → `EXPIRED`;
    4. `reserved_username` longer than 32 bytes, or a label that is not `<stem>.<2+ digits>` → `FAILED_TERMINAL`. `BaseLabel` is `BoundedVec<u8, ConstU32<32>>`. `MAX_BASE_LEN = 29` already keeps `base.NN` at ≤32, but `reservedUsername` is unbounded at intake today;
    5. `LiteLabelOwner[lite_label]` idempotency read. Owned by the candidate → `RESERVED` without submitting. Owned by anyone else → `FAILED_TERMINAL`. The direct analogue of the existing `username_owner` pre-check.
  - [x] Reconcile `dotns_status='SUBMITTING'` rows on writer startup against `LiteLabelOwner`, mirroring `reconcile_submitting`.
  - [x] `WriterConfig`: `dotns_gateway_enabled` (`DOTNS_GATEWAY_ENABLED`), `asset_hub_rpc_url` (`ASSET_HUB_RPC_URL`), One fail-fast abort at startup: enabled with no RPC URL, a config error knowable before any row is claimed. (The attester-identity abort this slice originally added is gone: #32's derived proxying makes the mismatch it guarded unrepresentable. The `reserve_name` shape assertion is no longer an abort either — it moved onto the lazy-connect path, where it parks the lane instead of the process.) Asset Hub connects on the first pass and re-dials every 30s while down, so an unreachable endpoint at boot never stalls People. Lane off → no AH connection is opened at all, and the writer logs `dotns lane disabled`.
  - [x] `just check` green.
- **✅ Demo — NOT YET RUN.** The local dev Postgres container was lost to a host disk-full event mid-slice. Recreate it first. Then `set -a; . .local/previewnet.env; set +a`, then `just run` + `just run-writer` and a registration through the CLI:
  - writer logs `registration assigned on-chain` **and** `dotns reserved on-chain` with an AH tx hash;
  - `DotnsGateway::LiteLabelOwner[<base>.<NN>]` on previewnet AH returns the candidate account;
  - `pop-dotns-testing`'s contract-side assertions (`checkLabelStored`, `checkChatKeyPersisted`, `checkNftOwnership`) pass for that label. That verifies from the *other* side of the dispatcher, not just from the pallet's own storage.
  This is exactly the run that fails today.
- **Risks / failure modes**:
  - The dispatcher contract reverts. `DispatcherAddress` is set: `0xa889cca3…` on previewnet, `0xd3f059fa…` on Paseo next. A revert is a dispatch error, so it lands as `RETRY_AFTER` and then `FAILED_TERMINAL`. The revert reason must reach `dotns_last_error` or it is undiagnosable.
  - `reserve_name` is `Pays::No` on success only. A run of failures is paid by the signer. The existing `ATTESTER_SIGNER_BALANCE_FLOOR_PLANCK` warning covers the People lane's fees, but the balance it reads is on People. AH balance is a new gauge (Phase 4).
  - Two lanes in one sequential loop double the worst-case time a batch holds the lease. Mid-submit renewal already covers it. `CHAIN_WRITER_BATCH_SIZE` × 2 × `CHAIN_WRITER_FINALIZE_SECS` is the new ceiling to keep in mind.

## Phase 4: boundaries, observability, and the deployed previewnet environment

- **Goal**: `https://identity-previewnet.dotspark.app` registers into dotNS; `https://identity.dotspark.app` is provably untouched.
- **Scope checklist**:
  - [x] `docker-compose.yml`: add `DOTNS_GATEWAY_ENABLED` and `ASSET_HUB_RPC_URL` to the `identity-chain-writer` allowlist (`ATTESTER_ACCOUNT` is already there). Reuse `CHAIN_WRITER_MAX_ATTEMPTS` / `CHAIN_WRITER_FINALIZE_SECS` for the AH lane. No parallel knobs until one is measurably needed.
  - [x] `scripts/verify_compose_boundaries.sh`: move `DOTNS_GATEWAY_ENABLED` out of the writer's `forbid_key` loop and `require_key` it on **both** `identity-api` and `identity-chain-writer`. That is the `QUEUE_ENABLED` precedent, for the same reason: a split value is exactly the intake-accepts-but-nothing-submits gap this slice exists to close. Keep the two intake-only knobs (`DOTNS_INTAKE_FRESHNESS_MAX_AGE_SECS`, `DOTNS_MAX_FUTURE_SKEW_SECS`) forbidden everywhere but `identity-api`. `require_key identity-chain-writer ASSET_HUB_RPC_URL` and `ATTESTER_ACCOUNT` (and the same on `identity-api`, which serves it). `forbid_key ASSET_HUB_RPC_URL` on every other service; it is not a secret, but the allowlists are the ownership map.
  - [x] Metrics, in the existing `log_attester_resources` / `record_outbox_gauges` pass. Same process, same exporter, so `observability/prometheus.yml` needs nothing. New series: `ibv2_dotns_outbox_depth{status}`, `ibv2_dotns_attester_allowance`, and `ibv2_account_free_balance_planck{role="signer",chain="asset-hub"}`. A `warn!` below the allowance floor, as People already has.
  - [x] Lane **off** by default, opt-in per environment. *(Superseded 2026-08-07 — see the amendment on the paseo-next-v2 item below. The shipped defaults are now a matched PreviewNet pair with the lane on; the code fallback stays off.)* `config.rs` and `writer.rs` both default `DOTNS_GATEWAY_ENABLED` to `false` (they must match, or intake accepts blocks nothing submits). `ASSET_HUB_RPC_URL=wss://previewnet.substrate.dev/asset-hub` (`pop-dotns-testing/endpoints.json`, `previewnet` profile) stays a `.env.example`/`docker-compose.yml` convenience default, but it is not an instruction to enable the lane. Rationale: `PEOPLE_RPC_URL` defaults to Paseo next, so an on-by-default lane paired that endpoint with PreviewNet's Asset Hub — attesting usernames on one network and claiming `LiteLabelOwner` for their labels on another. That claim is global and unrecoverable; only the client can re-sign. The deployed previewnet `.env` (`~/identity-backend-previewnet`) therefore sets `DOTNS_GATEWAY_ENABLED=true` explicitly. **Note this also turns that environment's intake on for the first time**, so new `202`s start carrying dotns blocks.
  - [ ] **Not done — server-side, and now the blocking half.** paseo-next-v2's `.env` needs an explicit `DOTNS_GATEWAY_ENABLED=false` **before** its next deploy. Off is no longer what omission gives you. Without it the writer connects to PreviewNet AH from a Paseo-People environment; with `ASSET_HUB_RPC_URL` pointed at Paseo AH instead, the boot assertion stops that writer rather than letting it submit a mis-shaped call. Either way it is the `.env` that has to say so.

    > **Amended 2026-08-07 — the defaults were flipped, and this item is now hard-blocking.** The repo defaults moved to a matched PreviewNet pair: `PEOPLE_RPC_URL` → `wss://previewnet.substrate.dev/people` and `DOTNS_GATEWAY_ENABLED` → `true`, in `.env.example` and in all seven `docker-compose.yml` services. The Rust `env_bool` fallback in `config.rs`/`writer.rs` deliberately **stays `false`**: a bare process has no URL pairing to make the lane safe, and every real path (compose, `.env`) passes the value explicitly. The opt-in assertion in `config.rs`'s minimal-environment test is unchanged and still passes.
    >
    > The consequence for paseo-next-v2 is no longer theoretical. Its `.env` (`docs/deploy.md` §2) overrides `PEOPLE_RPC_URL` to Paseo next but **not** `ASSET_HUB_RPC_URL`, so an omitted flag now yields exactly the pairing this plan calls unrecoverable: attest on Paseo People, claim `LiteLabelOwner` on PreviewNet's Asset Hub. The shape assertion does **not** save it — PreviewNet AH is the shape the writer expects, so the call succeeds. `docs/deploy.md` now carries `DOTNS_GATEWAY_ENABLED=false` inside the primary `.env` block with that reasoning; the server-side file still has to be edited by hand.
  - [x] `DOTNS_MAX_FUTURE_SKEW_SECS` default 600 → 30 in `.env.example`, `docker-compose.yml`, `config.rs` and `.local/previewnet.env`. In `config.rs` the `from_env` default only; `test_default()` stays 600 so the golden fixtures are untouched.
  - [x] Docs in the same change. `docs/architecture.md`: the writer now spans two chains and owns two nonce lanes, plus the dotNS status model. `docs/deploy.md` + `docs/runbook.md`: the new vars, the AH allowance and AH signer balance to watch, and what `DOTNS_EXPIRED` means. And `.env.example`.
  - [x] `just check` green.
  - [ ] **Not run locally.** `scripts/verify_compose_boundaries.sh` needs the `docker compose` plugin, which is not installed on this machine. Its new assertions were verified against the raw compose files by an equivalent parse: `DOTNS_GATEWAY_ENABLED` required on api **and** writer, `ASSET_HUB_RPC_URL`/`ATTESTER_ACCOUNT` required on the writer and forbidden elsewhere, both intake-only knobs still writer-forbidden. CI runs the real script.
- **✅ Demo — NOT YET RUN**:
  - `scripts/verify_compose_boundaries.sh` prints `compose configuration boundaries: ok`;
  - one deployable push to `main` updates both stacks and both health checks pass;
  - a registration through `https://identity-previewnet.dotspark.app` reaches `ASSIGNED` on previewnet People **and** `dotns_status='RESERVED'` with an AH tx hash. The label resolves through the dotNS contracts;
  - `https://identity.dotspark.app` is unchanged. `/readyz` healthy throughout, its writer logs `dotns lane disabled`, and its Asset Hub allowance (999,125) is **unmoved**.
- **Risks / failure modes**:
  - Turning intake on for deployed previewnet is a visible behaviour change for anything already pointed at it. A client sending a bad dotns block now gets a 400 where it got a 400 for a different reason before, the gateway-disabled message. Coordinate with whoever is using that hostname.
  - The boundary script's `forbid_key` loop is the thing that catches a config mistake here. Changing it is the one edit in this slice that weakens a guardrail. The `require_key` on both sides therefore has to land in the same commit.

## Phase 5: bound `reservedUsername` at intake (signature check reverted)

Ships alone. It is the only wire change in the slice.

- **Original goal**: a client learns immediately that its dotns signature will not be accepted, instead of getting a `202` and silently no dotNS name.
- **Reverted.** Verifying the reservation signature at intake turned an unverifiable *optional* block into a `400` that cost the caller its **People username** — a request that previously got a `202` and a working username now got neither. A client still signing an older reservation payload hits exactly that. It also contradicted the lane's own design: `ASSIGNED` + `DOTNS_FAILED_TERMINAL` is a documented legitimate resting state, so intake refusing what the writer is built to absorb was the wrong boundary. The writer's `check_dotns_submittable` still runs the identical check before spending an extrinsic, so nothing mis-shaped reaches the chain either way.
- **Scope checklist**:
  - [x] ~~`validate_register` refinement using the Phase 2 verifier. New `400` on `#/dotns/signature`.~~ Reverted; intake stores the block and the writer decides.
  - [x] Bound `reservedUsername` at 32 bytes. `BaseLabel` caps there. Today intake accepts any length ≥6, and a 33-byte value makes the extrinsic unbuildable at the writer. **This half stands**: it rejects a block that cannot be relayed at all, rather than one whose signature merely does not verify.
  - [x] ~~New fixture `register-400-dotns-signature`.~~ Removed with the revert; the captured fixture count is back to the frozen 55.
  - [x] `just openapi` re-ran, **no diff**. The change is validation logic and error detail strings, neither of which is in the schema.
  - [x] `just check` green.
- **Risks / failure modes**:
  - The legacy dialect is frozen and golden-captured, and after the revert this phase adds **no** 400 case legacy did not have. The `reservedUsername` bound is the one tightening left, and it only rejects blocks that were always unbuildable.

# Contracts (guardrails)

- **One writer per nonce lane, lane = (signing account, chain).** This slice adds a second *lane* to the same process and the same `writer_lease` row. It does not add a second process, a second lease name, or a second replica. `identity-chain-writer` stays `replicas: 1` and stays on `AGENTS.md`'s don't-scale list.
- A dotNS failure **never** changes the People `status`. `ASSIGNED` + `DOTNS_FAILED_TERMINAL` / `DOTNS_EXPIRED` is a legitimate resting state.
- `reserve_name` is never submitted for a row that is not `ASSIGNED` on People. The label is a global claim.
- `ATTESTER_ACCOUNT` equals the extrinsic's signed origin on both chains, by construction — proxying is derived from it, not configured alongside it.
- The writer submits exactly one `reserve_name` shape and asserts it against the connected Asset Hub's metadata at startup. It never detects a runtime and switches encodings. It never submits against a shape it did not recognise.
- An expired reservation signature is **never** retried and **never** submitted. The backend never fabricates or refreshes a candidate signature.
- No backfill. A row created before the migration is never submitted to Asset Hub.
- `chain-types` stays the only place chain types live. **One tuple per chain family**, not one overall. subxt's 26-member tuple ceiling makes the merge inexpressible; see the amendment under *One extension tuple per chain family*. Within a family, one tuple still serves every runtime.
- Every writer DB mutation stays behind the `LEASE_HELD` guard.
- Nothing new publishes a host port; `scripts/verify_compose_boundaries.sh` passes.
- `identity.dotspark.app` (paseo-next-v2) behaviour and its Asset Hub allowance are unchanged by every phase.

# Testing

**In CI** (`just check` — no chain, no Postgres):

- extension-set coverage for the two Asset Hub runtimes (`chain-types`, extends the existing snapshot test);
- the golden reservation-message vector plus the four negative cases (Phase 2);
- `build_reserve_name_tx` shape, direct and proxy-wrapped, mirroring `direct_registration_is_not_wrapped_in_utility_batch` / `proxied_registration_wraps_attest_directly`;
- the pre-submit gate classifier. Expired / bad signature / label-owned-by-other / oversized reserved label → the right terminal state; future-dated → deferred, not failed, and without spending an attempt; only transient reasons → retry;
- the dotNS lane's dial state machine: backoff between failed Asset Hub dials, and one log line per distinct cause rather than one per pass;
- `assert_reserve_name_shape` over both field-name lists captured 2026-08-06. PreviewNet's accepted, Paseo next's rejected with the offending fields named. A literal list, like `KNOWN_RUNTIMES`, so no metadata is vendored;
- the unchanged replay of the existing dotns fixtures, and that an unverifiable `dotns.signature` still yields a username (Phase 5).

**Live Postgres**, `#[ignore]`-gated on `IDENTITY_TEST_DATABASE_URL` exactly like `queue_live.rs`. A new `crates/identity-service/tests/dotns_live.rs` covers the new columns, `claim_dotns_due` (only `ASSIGNED` + `PENDING`/`RETRY_AFTER`), the lease-guarded transitions, and the no-backfill property: a row inserted with `dotns_status` left `NULL` is never claimed.

**Live chain — deliberately not in CI.** AH submission needs a funded attester holding gateway allowance, and CI has neither. That is the same reason People submission is not in CI today. It is covered instead by:

- the Phase 1 finalized-remark demo. It proves the encoding and needs only a funded account;
- the Phase 3 local previewnet run, cross-checked with `pop-dotns-testing`'s contract-side assertions. Those read the dotNS contracts from the opposite side of the dispatcher, and would catch a pallet write that never reached the contract;
- the Phase 4 deployed demo.

What that leaves uncovered, stated rather than hidden: nothing automated fails if previewnet AH is re-wiped or the attester loses its allowance. The `ibv2_dotns_attester_allowance` gauge and its floor warning are the standing signal.

# Later / Deferred

- **paseo-next-v2 (`identity.dotspark.app`) — blocked on a Paseo AH runtime upgrade, not on us.** Its Asset Hub (`2000033`) still runs the pre-`signed_at` `reserve_name`, which takes `ring_vrf_key` + `proof_of_ownership` and has no freshness window. The stored payload does not fit it, and the boot assertion will refuse the environment until it does. Everything *else* is already provisioned: `5F7H1LkZ…` holds 999,125 AH allowance, the writer signer is its `Any`/delay-0 proxy on both chains, and it is funded. When Paseo AH reaches the PreviewNet gateway, enabling it is `ASSET_HUB_RPC_URL=wss://paseo-asset-hub-next-rpc.polkadot.io` plus a finalized Paseo AH extrinsic as the gate. Its stored-but-never-submitted backlog is all beyond the 3-day window, so `NULL` stays the right resting state. **Do not** implement the old shape in the meantime.
- **Watching for the upgrade.** Nothing alerts when Paseo AH gains the new gateway. Cheapest signal: re-run `assert_reserve_name_shape` against it by hand when someone next asks. A periodic check is not worth a job.
- **A client-facing re-claim path for `DOTNS_EXPIRED`**, where the client re-signs and re-submits through a refresh route. Not needed while intake freshness is 600 s against a 3-day window and the writer drains in seconds. Revisit the first time the metric is non-zero.
- **Full-person registration** (`register_name` + `AsDotnsGateway` + ring proofs). Different extrinsic, different origin, different proof material.
- **dotNS reads in `username-indexer`** (deferred 2026-08-06 — worth a GitHub issue). The read side is the mirror of the write-side gap. The indexer projects **People `Resources::Consumers` only**: one `OnlineClient<PeopleConfig>` in `crates/username-indexer/src/chain.rs`, decoded by `projection.rs:63`. Clients meanwhile resolve dotNS. Three candidate sources, only one buildable:
  - `DotnsGateway::LiteLabelOwner` — `BaseLabel → AccountId32`. No chat key, no full name, no prefix index. A reservation ledger, not a directory. Useful only for the point lookup this slice already uses.
  - **`NameReserved` / `NameRegistered` events** — `NameReserved` carries `candidate, attester, lite_label, chat_key, reserved_base_label`, i.e. every field an `assigned_usernames` row needs (`chat_key` *is* the `identifier_key`), with `full_username` from `NameRegistered`. Same finalized-block loop, different pallet on a different chain. This is the path if it is ever built.
  - The dotNS contracts themselves — six `ReviveApi_call` dry-runs per account (truapi's `resolve_labels`) with **no enumeration primitive**. Ruled out for a prefix search over ~37 k rows.

  Deferred because measured 2026-08-06 previewnet AH has seen ~9 `reserve_name` calls ever against ~174 People lite registrations. People is therefore a strict superset, and indexing dotNS would add zero rows today. Revisit when a second attester writes to the same Asset Hub. The divergence this slice *creates* runs the other way: a row that ends `ASSIGNED` + `DOTNS_FAILED_TERMINAL`/`EXPIRED` is advertised by search but unresolvable in dotNS. That is a cross-database concern, watched by `ibv2_dotns_outbox_depth`, not an indexer read.
- **A typed Asset Hub codegen** in `chain-types`. Revisit if a second crate needs the AH surface, or if the dynamic call ever fails in a way types would have caught.
- **Separate AH lane tuning** (`DOTNS_MAX_ATTEMPTS`, `DOTNS_FINALIZE_SECS`). Revisit if the two lanes' timings measurably diverge.
