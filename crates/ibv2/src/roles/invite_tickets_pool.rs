//! `invite-tickets-pool` — the single-instance pool maintainer.
//!
//! Keeps each configured `(dim, network)` pool stocked: generates sr25519
//! keypairs, registers their public keys on People Chain
//! (`set_invite_ticket` batches signed as the inviter, optionally via proxy),
//! and inserts them as `available` only after finalization. **Never scale
//! this past one replica** — one inviter = one nonce lane; a Postgres
//! advisory lock additionally guards deploy overlaps.

use anyhow::Context as _;

use invite_tickets::pool::{self, MaintainerConfig};

pub async fn run() -> anyhow::Result<()> {
    http_common::telemetry::init("invite-tickets-pool");
    http_common::metrics::spawn("invite-tickets-pool");

    let config = MaintainerConfig::from_env().context("invalid configuration")?;

    // The silent `signal()`, not `drain()`: this role logs its own message.
    // Interrupting between a submission and its inserts is safe — the batch's
    // tickets are simply discarded (never became claimable), which is the
    // failure-path behavior.
    tokio::select! {
        result = pool::run(config) => result,
        _ = crate::shutdown::signal() => {
            tracing::info!("shutdown signal received; stopping maintainer");
            Ok(())
        }
    }
}
