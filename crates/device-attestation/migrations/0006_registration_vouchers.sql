-- Registration vouchers — the INSTANT lane of the eligibility slice
-- (docs/plans/active/eligibility-payment.md). An admin-minted, single-use,
-- expiring key submitted as `lifetimePoUDVoucher` on POST /api/v1/usernames
-- bypasses the PoUD gate and the registration queue.
--
-- Hash-only at rest (threat-model finding M1, deliberate hardening over the
-- legacy plaintext `key` column): `key_hash = sha256(key)`; the plaintext key
-- exists exactly once, in the voucher-mint CLI's stdout. Any read access to
-- this table therefore yields nothing redeemable.

CREATE TABLE registration_vouchers (
    -- sha256 of the distributed voucher key (32 bytes).
    key_hash     BYTEA       PRIMARY KEY,
    -- Operator audit label: which mint run produced this voucher.
    minted_batch TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Hard validity bound; a voucher past this instant is never redeemable.
    expires_at   TIMESTAMPTZ NOT NULL,
    -- Single-use consumption mark, set atomically with the reservation insert.
    used_at      TIMESTAMPTZ
);
