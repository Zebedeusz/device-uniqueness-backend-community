> Stage: done. Built and verified 2026-08-06 against self-generated proofs; the route stays behind
> `TURN_PROOF_ENABLED=false` in every environment until a proof from a shipping host verifies
> against the target runtime (see Remaining externals).

# Personhood-proof TURN issuance

Issues: `identity-backend-rust#21`, `identity-backend#2904`.

## Shipped behavior

- `POST /api/v1/turn/issue-with-proof` accepts a valid Lite/Full ring-VRF proof plus its required
  `collection`, `ringIndex`, `ringRevision`, and `timestamp` — without an
  app-integrity JWT, and returns the same 201 credential body as `POST /api/v1/turn/issue`.
- Proof bytes are what a host emits: the raw ring-VRF signature, with no SCALE length prefix.
- The server derives the proved message itself as `blake2b256(label ‖ timestamp)`,
  so no challenge round trip exists and nothing about the request is stored. The
  product builds the same digest; it only needs a clock.
- Redemption is a single request. There is no `/challenges` route.
- The proof credential id is a keyed, opaque derivation over product, contextual alias, and the
  expiry timestamp (`now + TURN_TTL_SECS`). The returned `ttl` is the configured TTL in whole
  seconds. The JWT route keeps random ids and a fresh TTL.
- The proof route is behind `TURN_PROOF_ENABLED` and is absent when it is off. The original JWT
  route is unchanged and remains available when the People Chain RPC is unavailable.
- Two background refreshers cache each canonical collection's current roots and retained previous
  revisions. Requests never read the chain directly; an empty selected cache fails proof redemption
  with 503 while preserving the JWT route.
- Redemption applies an 8 KB body cap, a ±60s timestamp window, CPU-aware bounded verification
  concurrency, up to 64 requests waiting 50ms for a permit, blocking-task proof verification, and
  a post-verification per-alias throttle. Unknown ring/revision pairs are rejected before admission.
  There is deliberately no pre-verification rate limit across all callers: with no authenticated
  subject to key on, a shared counter limits honest callers rather than an attacker.

## Key decisions and invariants

- The server selects the accepted products, canonical collection allowlist, genesis, ring domains,
  and accepted roots. The request repeats the 32-byte hex collection id from TrUAPI's
  `ringLocation`; only `pop:polkadot.network/people-lite` and `pop:polkadot.network/people` are
  accepted, and the selected collection never falls through to the other cache. Required
  `ringIndex` and `ringRevision` together name one server-held root, so a request costs at
  most one ring verification and a revision the server no longer holds is refused without verifying
  anything; neither field can introduce caller-supplied root material. The client always has the
  revision — the host returns `ring_revision` alongside the proof — and both numbers read the same
  `Members::Root` field, so the labels cannot disagree.
- Contexts are per product, not one shared pin. `TURN_PROOF_PRODUCTS` lists the accepted product
  ids and the server derives each context itself as
  `blake2b256("product/" ‖ productId ‖ "/" ‖ indexBytes(TURN_PROOF_CONTEXT_SUFFIX))` — the same
  derivation the hosts use. Each request names the `productId` it proves for; an
  unlisted one is refused before verification. A product-scoped context is the only shape TrUAPI
  can express, so a shared pin was never an option; RFC-0024 removed the user prompt this plan
  originally cited and gates a foreign key on the owner's manifest allowlist instead. The cost is
  that the alias, being context-derived, splits a person's budget per product.
- Freshness is client-asserted and server-bounded: the client picks the timestamp, and
  the server rebuilds the digest and bounds it. A captured proof stays usable for at most the skew
  window.
- **No client-chosen nonce, and no genesis in the digest.** The product builds this
  message, so it contains only a timestamp plus the `ibv2/turn-credential/v1` label
  (never shipped, so the earlier nonce/genesis layouts were never redeemed). Genesis
  stays a server pin (`TURN_PROOF_GENESIS`) for the root refresher: a proof made on
  another chain fails because that ring root does not verify. The nonce existed only
  to reserve a wire field for a future seen-value set, and that reservation is
  unnecessary: ring-VRF proofs are deterministic — `ark-vrf` derives the Pedersen
  blinding and both nonces from the secret and the transcript, and its only `OsRng`
  use is the `secret-split` side-channel countermeasure, which does not change the
  output. A person's proof for one second is therefore one fixed value, so
  single-use keys on `(alias, timestamp)`, both of which the server already derives.
  That key is also strictly stronger: a client-chosen nonce only catches a captured
  copy, because its holder can vary it to mint unlimited distinct proofs for the
  same second. The cost is that two redemptions in the same second are
  byte-identical requests, so a single-use rule would cap a person at one per
  second — ample against a 30-minute credential TTL that one credential set already
  covers every peer connection with.
- The contextual alias is an in-memory throttle key and an input to a domain-separated HMAC-SHA256
  credential-id derivation keyed by `TURN_SECRET`. It never appears in responses, logs, or metrics,
  and cannot be recovered from the 16-byte opaque id in an issued username.
- One TURN process serves one environment. The caller-supplied `productId` selects only among
  server-derived contexts; no caller-controlled environment header or hostname-to-product map was
  added, and naming another product buys nothing without a proof under that product's context.
- Root refresh is single-writer per collection, paced after every failure, and off the request path.
  Each last valid snapshot remains usable during RPC outages; genesis mismatches are rejected by
  the refreshers, not by the proved message.
- The verifier remains private to `turn` until a second production consumer exists.
- `verifiable` is pinned to `1f9f67524acce0d9c46dbbb566f84ce52757b114`, matching the
  `individuality` runtime dependency. The root reader deliberately supports only that 288-byte
  commitment encoding and fails clearly on older roots.

## Verification evidence

- Phase 0: a locally generated proof verified against a self-attested PreviewNet `people-lite`
  member and a bit-flipped proof was rejected. Rebuilding the commitment from public
  `Members::RingKeys` byte-matched the live root.
- Router integration tests cover success, tampered proof, another accepted product's context, an
  unlisted product, per-product budget isolation for one person, a proof over a different in-window
  timestamp than the request names, a stale timestamp, required/wrong ring
  index, a missing ring revision, revision selection across two held roots, wrong-length and
  SCALE-framed proof bytes, both canonical collection caches, rejection of any other collection,
  deterministic credentials across two byte-identical requests, per-alias isolation, saturation,
  missing root snapshot, and the disabled route.
- Live PreviewNet flow: a self-generated proof redeemed at `issue-with-proof` returned 201 without
  an `Authorization` header. Starting with a dead RPC preserved 201 on `/turn/issue` and failed
  redemption cheaply.
- Root rotation: the retained previous revision returned 201; a two-revisions-old proof returned
  403. Runtime `OldRoots` retention bounds this window, so clients should prove promptly.
- Real relay: proof-issued credentials completed a STUN Allocate against
  `turn-dev-b.novasama-tech.org:3478` and received relay candidate `51.15.72.62:59916`.
- The live PreviewNet and relay runs above predate the digest simplifications (they
  exercised `label ‖ genesis ‖ timestamp ‖ nonce`). Only the digest inputs changed —
  ring selection, context derivation, verification, and the 201 body are untouched —
  and the router tests cover the current `label ‖ timestamp` layout, but the live
  flow has not been re-run since.
- `just openapi` regenerated the API reference and `just check` passed.

## Remaining externals

- No shipping host can produce a proof yet. Desktop, Android, iOS, and web all answer
  `Account/create_account_proof` with `Not implemented`, while `Account/get_account_alias` is
  implemented on all four — so the hosts already reach the person key and only the proof handler is
  missing. The TrUAPI signing-host and pairing-host CLIs implement the call and are the only
  source of a real host proof today. Validate Nova client interoperability with a pre-enrolled
  Lite/Full person and coordinate client adoption once a host ships the handler.
- The host contract that scopes proofs to keys a product owns is RFC-0024
  (`truapi#324`, implementation `truapi#360`, host SDK `useragent-kit#1653` — all open). Under it a
  caller naming a key it does not own needs the owner's manifest allowlist, with no user-prompt
  fallback, and that allowlist has no mechanism until the manifest RFC (`truapi#206`) lands. The
  context derivation is unchanged by RFC-0024, so verification stays byte-identical; only
  reachability is affected.
- Agree the production product ids for `TURN_PROOF_PRODUCTS` with the host/product owner. The
  verifier takes the host's raw ring-VRF bytes, so redeeming a captured host proof is what
  confirms both the framing and the `verifiable` revision skew (TURN pins `1f9f675`, the host
  stack `19b03de`).
- Deploy only against a runtime whose rings have re-rooted with the pinned 288-byte commitment
  encoding. PreviewNet was compatible during verification; Paseo Next v2 spec `1000029` was not.
