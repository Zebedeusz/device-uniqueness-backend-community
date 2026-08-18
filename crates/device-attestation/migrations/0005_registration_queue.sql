-- Registration queue (the spec's balance-priority FIFO): with QUEUE_ENABLED,
-- claims enter the outbox as QUEUED rows ahead of RESERVED. The queue advancer
-- (in the device-attestation-chain-writer process) promotes up to 4 rows per iteration to
-- RESERVED; the writer path is unchanged from there.

-- Priority group 1..4 derived from the subject account's on-chain balance
-- (<10 DOT -> 1, >=10 -> 2, >=100 -> 3, >=1000 -> 4); refreshed while queued.
-- The CHECK is a schema belt for the invariant: both insert paths bind 1..=4,
-- but the promotion SQL compares the raw column while the API snapshot clamps
-- into 1..=4 — an out-of-range value (manual edit, future migration bug)
-- would make the status endpoint report a position for a row the advancer's
-- slots can never select. Reject it at the schema instead.
ALTER TABLE username_reservations
    ADD COLUMN queue_group INTEGER NOT NULL DEFAULT 1
    CONSTRAINT username_reservations_queue_group_range
    CHECK (queue_group BETWEEN 1 AND 4);

-- Advancer slot scan + queue-status snapshot: QUEUED rows in FIFO order.
CREATE INDEX username_reservations_queued_idx
    ON username_reservations (created_at, id)
    WHERE status = 'QUEUED';
