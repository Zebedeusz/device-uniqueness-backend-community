-- Chain identity of the projection the checkpoint belongs to.
--
-- `sync_state` recorded how far indexing had got, but never *which chain* that
-- block number came from. A wiped or repointed chain therefore resumed against
-- a checkpoint belonging to a chain that no longer exists: `ensure_seeded`
-- skips the full scan whenever the row is present, and the incremental pass
-- no-ops while the new chain's head is below the stale number. The projection
-- kept serving the dead chain's usernames with every health signal green.
--
-- Nullable and deliberately not backfilled. No genesis hash can be
-- reconstructed for a row written before this column existed, and inventing a
-- default would either be wrong or would condemn every live projection to a
-- full rebuild on the deploy that adds the column. NULL therefore means
-- "indexed before the guard existed": the first boot after this migration
-- stamps the connected chain's genesis and keeps the projection, and only a
-- later *mismatch* against a stamped value discards anything.
ALTER TABLE sync_state
    ADD COLUMN genesis_hash BYTEA
    CONSTRAINT sync_state_genesis_hash_len
    CHECK (genesis_hash IS NULL OR octet_length(genesis_hash) = 32);
