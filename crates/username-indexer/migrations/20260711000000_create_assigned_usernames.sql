CREATE TABLE assigned_usernames (
    account_id BYTEA PRIMARY KEY CHECK (octet_length(account_id) = 32),
    account_id_ss58 TEXT NOT NULL,
    identifier_key BYTEA NOT NULL CHECK (octet_length(identifier_key) = 65),
    lite_username TEXT NOT NULL,
    lite_base TEXT NOT NULL,
    lite_digits NUMERIC NOT NULL CHECK (lite_digits >= 0),
    full_username TEXT,
    display_username TEXT NOT NULL,
    snapshot_hash BYTEA NOT NULL CHECK (octet_length(snapshot_hash) = 32),
    snapshot_number BIGINT NOT NULL CHECK (snapshot_number >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX assigned_usernames_prefix_idx
    ON assigned_usernames ((lower(display_username) COLLATE "C") text_pattern_ops);

CREATE INDEX assigned_usernames_order_idx
    ON assigned_usernames (
        (lower(display_username) COLLATE "C"),
        (lite_base COLLATE "C"),
        lite_digits,
        account_id
    );