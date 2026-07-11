CREATE TABLE sync_state (
    id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    last_finalized_number BIGINT NOT NULL CHECK (last_finalized_number >= 0),
    last_finalized_hash BYTEA NOT NULL CHECK (octet_length(last_finalized_hash) = 32),
    last_synced_at TIMESTAMPTZ NOT NULL,
    records_indexed BIGINT NOT NULL CHECK (records_indexed >= 0),
    decode_failures BIGINT NOT NULL CHECK (decode_failures >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
