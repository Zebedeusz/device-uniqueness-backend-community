-- Payment requests — the PAYMENT_REQUIRED lane of the eligibility slice
-- (docs/plans/active/eligibility-payment.md, Phase 2). A claim blocked by the
-- device gate (seen device / missing token in hard mode) gets a unique
-- deposit address + required amount; the row stores the fully-validated claim
-- so a confirmed deposit (Phase 3 watcher) can insert the reservation with no
-- further client action.
--
-- The payment address is the threshold-1 multisig account of the cold master
-- and a keyless per-subject dummy
-- (`multi_account_id(sort([master, blake2_256(TAG ‖ subject)]), 1)`): one
-- deterministic address per subject, no private key server-side, and — the
-- dummy being a hash output no key exists for — dispatchable by the master
-- alone, who sweeps offline with `Multisig.as_multi_threshold_1` after
-- re-deriving the dummy from the row's account_id. Nothing is allocated, so
-- there is no index space to exhaust. Deliberate consequence (recorded
-- 2026-08-03): a subject's successive requests share the address, so under
-- cumulative-balance detection any unswept balance counts toward the next
-- quote — sweep cadence is the mitigation.

CREATE TABLE payment_requests (
    id                                BIGSERIAL   PRIMARY KEY,
    -- Authenticated JWT subject the quote belongs to.
    account_id                        TEXT        NOT NULL,
    -- PENDING -> CONFIRMED (deposit observed, reservation inserted)
    --         -> EXPIRED (TTL passed unpaid)
    --         -> FAILED_CONFLICT (paid, but the username could not be
    --            reserved; kept for support — money was observed).
    status                            TEXT        NOT NULL DEFAULT 'PENDING',
    -- SS58 of the multisig deposit account (denormalised for display/audit;
    -- derivable from master + account_id). NOT unique: one subject's
    -- historical requests all carry the same address by construction.
    payment_address                   TEXT        NOT NULL,
    -- Required deposit in planck, frozen at quote time.
    amount_planck                     BIGINT      NOT NULL CHECK (amount_planck > 0),
    expires_at                        TIMESTAMPTZ NOT NULL,
    confirmed_at                      TIMESTAMPTZ,

    -- The validated claim intent (NewReservation minus the digit selection —
    -- the discriminator is re-selected at confirmation time so a quote held
    -- for days rarely conflicts; `preferred_digits` is honored if still free).
    candidate_account_id              TEXT        NOT NULL,
    base                              TEXT        NOT NULL,
    preferred_digits                  TEXT,
    candidate_signature               BYTEA       NOT NULL,
    ring_vrf_key                      BYTEA       NOT NULL,
    proof_of_ownership                BYTEA       NOT NULL,
    consumer_registration_signature   BYTEA       NOT NULL,
    identifier_key                    BYTEA       NOT NULL,
    dotns_signature                   BYTEA,
    dotns_signed_at                   BIGINT,
    reserved_username                 TEXT,

    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One ACTIVE quote per subject: a re-claim returns (and re-targets) the
-- existing pending request instead of minting a second one.
CREATE UNIQUE INDEX payment_requests_one_pending_per_subject
    ON payment_requests (account_id)
    WHERE status = 'PENDING';

-- Phase-3 watcher scan: pending requests by expiry.
CREATE INDEX payment_requests_pending_idx
    ON payment_requests (expires_at)
    WHERE status = 'PENDING';
