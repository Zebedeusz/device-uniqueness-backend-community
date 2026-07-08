//! `identity-chain-writer` — single-instance worker binary. Loads [`WriterConfig`] and
//! drains the reservation outbox onto People Chain. See [`identity_service::chain::writer`].

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use identity_service::chain::writer::{self, WriterConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = WriterConfig::from_env().context("invalid identity-chain-writer configuration")?;
    writer::run(config).await
}
