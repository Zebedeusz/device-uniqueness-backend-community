-- Best-effort deploy-overlap guard for the single-instance dim-tickets-writer
-- (same shape as identity-service's writer_lease). One inviter key = one nonce
-- lane; the lease is not a fence — the chain nonce plus the retry_at lease on
-- SUBMITTING rows is the real serializer.
CREATE TABLE writer_lease (
    name         TEXT        PRIMARY KEY,
    holder_id    TEXT        NOT NULL,
    lease_epoch  BIGINT      NOT NULL DEFAULT 0,
    expires_at   TIMESTAMPTZ NOT NULL,
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
