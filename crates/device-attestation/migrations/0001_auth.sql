-- device-attestation auth foundation: single-use TTL challenges + rotating refresh tokens.

CREATE TABLE auth_challenges (
    challenge   BYTEA       PRIMARY KEY,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ
);

-- Sweep helper for expired, never-consumed challenges.
CREATE INDEX auth_challenges_expires_at_idx ON auth_challenges (expires_at);

CREATE TABLE refresh_tokens (
    token                   TEXT        PRIMARY KEY,
    account_id              TEXT        NOT NULL,
    app_from_official_store BOOLEAN     NOT NULL DEFAULT true,
    platform                TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at              TIMESTAMPTZ NOT NULL,
    used_at                 TIMESTAMPTZ,
    replaced_by             TEXT
);

CREATE INDEX refresh_tokens_account_id_idx ON refresh_tokens (account_id);
