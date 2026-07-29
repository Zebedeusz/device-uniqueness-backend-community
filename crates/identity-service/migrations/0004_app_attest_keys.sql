-- App Attest device keys registered via POST /api/v1/auth/app-attest/attestations.
--
-- Keys are deliberately NOT bound to an account: the App Attest key is
-- per-install while the account restores from iCloud Keychain, so a durable
-- binding would break reinstall/restore and account-switch flows. The
-- per-request account link is cryptographic (the assertion signs
-- challenge || clientId || sha256(body)).
--
-- Numbered 0004 to leave 0003 for the in-flight usernames-rewrite branch.

CREATE TABLE app_attest_keys (
    key_id                BYTEA       PRIMARY KEY,
    -- Uncompressed SEC1 P-256 public key (65 bytes) from the credential cert.
    public_key            BYTEA       NOT NULL,
    -- Apple receipt from the attestation object (kept for later fraud checks).
    receipt               BYTEA       NOT NULL,
    -- Last accepted assertion counter; assertions must strictly increase it.
    sign_count            BIGINT      NOT NULL DEFAULT 0,
    -- Observability only, never enforced.
    registering_client_id BYTEA,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_asserted_at      TIMESTAMPTZ
);
