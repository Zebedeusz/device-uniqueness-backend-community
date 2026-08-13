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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    http_common::telemetry::init("invite-tickets-pool");
    http_common::metrics::spawn("invite-tickets-pool");

    let config = MaintainerConfig::from_env().context("invalid configuration")?;

    tokio::select! {
        result = pool::run(config) => result,
        _ = shutdown_signal() => {
            tracing::info!("shutdown signal received; stopping maintainer");
            Ok(())
        }
    }
}

/// Resolve on Ctrl-C or (on Unix) `SIGTERM`. Interrupting between a
/// submission and its inserts is safe: the batch's tickets are simply
/// discarded (never became claimable), which is the failure-path behavior.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
