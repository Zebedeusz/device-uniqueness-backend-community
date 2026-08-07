-- dotNS gateway lane. A second, independent state machine on the same row.
--
-- `status` tracks the People registration (PeopleLite.attest). `dotns_status`
-- tracks the Asset Hub reservation (DotnsGateway.reserve_name). They advance
-- independently. A dotNS failure NEVER changes `status`. ASSIGNED +
-- DOTNS_FAILED_TERMINAL is a legitimate resting state. The username works, the
-- dotNS name does not.
--
--   NULL       -> the request carried no dotns block, OR the row predates this
--                 migration. Deliberately ambiguous. Backfilling would make the
--                 first writer boot after deploy fire a burst of Asset Hub
--                 extrinsics. Those signatures are almost all past the 3-day
--                 MaxValiditySeconds window. `created_at` separates the two
--                 cases. An operator can hand-flip a recent row to PENDING.
--   PENDING    -> has a dotns block. Waiting for the People row to reach
--                 ASSIGNED and for the pre-submit gates to pass.
--   SUBMITTING -> extrinsic built, signed and broadcast. Recorded before
--                 awaiting inclusion, so a crash reconciles against
--                 LiteLabelOwner instead of resubmitting.
--   RESERVED   -> confirmed on Asset Hub. Terminal success.
--   RETRY_AFTER-> transient failure. Re-enqueued behind dotns_not_before.
--   FAILED_TERMINAL -> bad signature, label owned by another account, or an
--                 unbuildable call. Terminal.
--   EXPIRED    -> signed_at aged out of MaxValiditySeconds before submission.
--                 Terminal, never retried. Only the client holds the candidate
--                 key. The backend can never re-sign.
--   ABANDONED  -> the People half reached FAILED_TERMINAL, so this half was
--                 never attempted. Terminal. Distinct from FAILED_TERMINAL:
--                 nothing is wrong with the reservation, there is just no
--                 username to attach a name to. Written by the same guarded
--                 UPDATE that fails the People half. Without it these rows sit
--                 in PENDING forever — unclaimable, since claim_dotns_due
--                 requires status='ASSIGNED' — and inflate the PENDING depth
--                 and oldest-age gauges into a permanent false "stuck lane".
ALTER TABLE username_reservations
    ADD COLUMN dotns_status     TEXT,
    ADD COLUMN dotns_tx_hash    TEXT,
    ADD COLUMN dotns_attempt    INTEGER     NOT NULL DEFAULT 0,
    ADD COLUMN dotns_not_before TIMESTAMPTZ,
    ADD COLUMN dotns_last_error TEXT;

-- Claim scan for the dotNS lane. Covers rows already on People whose gate has
-- passed. Partial, because the overwhelming majority of rows are NULL or
-- terminal.
CREATE INDEX username_reservations_dotns_claimable_idx
    ON username_reservations (created_at, id)
    WHERE dotns_status IN ('PENDING', 'RETRY_AFTER');
