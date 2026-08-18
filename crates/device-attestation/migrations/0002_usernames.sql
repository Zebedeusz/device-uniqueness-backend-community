-- device-attestation username write path: the reservation outbox + the single-writer lease.
--
-- One row per registration intent. `POST /api/v1/usernames` inserts a RESERVED
-- row and returns 202; the device-attestation-chain-writer claims it, submits PeopleLite.attest, and
-- advances the state. The row is the source of truth; chain is reconciled to it.

CREATE TABLE username_reservations (
    id                                BIGSERIAL   PRIMARY KEY,
    -- Authenticated JWT subject (0x-hex sr25519). Equals `candidate_account_id`.
    account_id                        TEXT        NOT NULL,
    -- Beneficiary SS58 address (the on-chain `candidate` / consumer account).
    candidate_account_id              TEXT        NOT NULL,
    base                              TEXT        NOT NULL,
    digits                            TEXT        NOT NULL,
    -- "base.digits" — the full on-chain lite username; globally unique (idempotency key).
    full_username                     TEXT        NOT NULL UNIQUE,

    -- Attestation payload, relayed verbatim into the on-chain call.
    candidate_signature               BYTEA       NOT NULL,
    ring_vrf_key                      BYTEA       NOT NULL,
    proof_of_ownership                BYTEA       NOT NULL,
    consumer_registration_signature   BYTEA       NOT NULL,
    identifier_key                    BYTEA       NOT NULL,

    -- Optional DotNS reservation (parks a base label for later full-person registration).
    dotns_signature                   BYTEA,
    dotns_signed_at                   BIGINT,
    reserved_username                 TEXT,

    -- Outbox state machine: RESERVED | SUBMITTING | ASSIGNED | RETRY_AFTER | FAILED_TERMINAL.
    status                            TEXT        NOT NULL DEFAULT 'RESERVED',
    -- Submit bookkeeping, set when a row enters SUBMITTING.
    tx_hash                           TEXT,
    nonce                             BIGINT,
    attempt                           INTEGER     NOT NULL DEFAULT 0,
    -- Retry gate for RETRY_AFTER (backoff); NULL means claimable now.
    not_before                        TIMESTAMPTZ,
    last_error                        TEXT,
    submitted_at                      TIMESTAMPTZ,
    created_at                        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Claim scan: pick RESERVED / RETRY_AFTER rows whose gate has passed.
CREATE INDEX username_reservations_claimable_idx
    ON username_reservations (status, not_before);

-- Single active device-attestation-chain-writer coordination. Best-effort deploy-overlap guard; the
-- chain nonce + reconciliation is the real serializer, not this lease.
CREATE TABLE writer_lease (
    name         TEXT        PRIMARY KEY,
    holder_id    TEXT        NOT NULL,
    lease_epoch  BIGINT      NOT NULL DEFAULT 0,
    expires_at   TIMESTAMPTZ NOT NULL,
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
