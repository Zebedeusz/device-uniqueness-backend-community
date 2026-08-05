//! `dim-tickets-writer` — the single-instance worker that registers pending
//! DIM tickets on People Chain.
//!
//! Fetches due rows, batches them into `Utility.force_batch` of
//! `Game`/`ProofOfInk` `set_invite_ticket` calls signed by the inviter key
//! (optionally as proxy of a cold primary), and advances the state machine
//! `PENDING → SUBMITTING → REGISTERED | PENDING(retry) | FAILED`.
//!
//! Single-instance by design: one inviter key = one nonce lane. Never scale it.

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use dim_tickets::writer::{self, WriterConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    http_common::metrics::spawn("dim-tickets-writer");

    let config = WriterConfig::from_env().context("invalid configuration")?;
    tracing::info!(
        poll_secs = config.poll_interval.as_secs(),
        batch = config.batch_size,
        max_retries = config.max_retries,
        "starting dim-tickets-writer"
    );

    writer::run(config).await
}
