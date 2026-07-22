-- DIM tickets: one row per user-submitted SS58 address, advancing
-- PENDING -> SUBMITTING -> REGISTERED | PENDING(retry) | FAILED.
-- The DB row is the source of truth; chain is reconciled to it.
--
-- Columns mirror the legacy `polkadot_app.dim_tickets` table (fresh start,
-- decided 2026-07-16) so a later one-time backfill stays a plain
-- INSERT ... SELECT. Deliberate deltas from legacy:
--   * no `registered` boolean — it is derived (status = 'REGISTERED') and the
--     wire mapping computes it; legacy had to backfill one from the other.
--   * no `trace_id` / `span_id` — legacy OTel plumbing, not wire contract.
--   * timestamptz instead of naive timestamp — this service's own data.
CREATE TABLE dim_tickets (
    -- SS58 address the ticket is issued to. PRIMARY KEY: one ticket per
    -- address, across DIMs, forever (legacy behavior — preserve, don't "fix").
    ticket text PRIMARY KEY,
    inviter text NOT NULL,
    network text NOT NULL,
    dim text NOT NULL,
    status text NOT NULL DEFAULT 'PENDING',
    onchain_data jsonb,
    retry_at timestamptz,
    retry_count integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz
);

CREATE INDEX dim_ticket_network_idx ON dim_tickets (network);
CREATE INDEX dim_ticket_dim_idx ON dim_tickets (dim);
CREATE INDEX dim_ticket_status_idx ON dim_tickets (status);
CREATE INDEX dim_ticket_inviter_idx ON dim_tickets (inviter);
CREATE INDEX dim_ticket_retry_at_idx ON dim_tickets (retry_at);
