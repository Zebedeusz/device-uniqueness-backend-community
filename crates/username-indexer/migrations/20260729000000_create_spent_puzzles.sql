-- Proof-of-compute replay guard: one row per spent puzzle session.
--
-- Issuance is stateless (HMAC-signed), so this table is written only when a
-- solution is accepted. Rows are pruned once their validity window has passed;
-- an expired row can never admit a replay because verification rejects the
-- puzzle on expiry before the consume.
CREATE TABLE IF NOT EXISTS spent_puzzles (
    session_id UUID PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS spent_puzzles_expires_at_idx ON spent_puzzles (expires_at);
