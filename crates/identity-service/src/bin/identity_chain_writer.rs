//! `identity-chain-writer` — single-instance worker binary. Loads [`WriterConfig`] and
//! drains the reservation outbox onto People Chain. See [`identity_service::chain::writer`].

use anyhow::Context as _;

use identity_service::chain::writer::{self, WriterConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    http_common::telemetry::init("identity-chain-writer");
    http_common::metrics::spawn("identity-chain-writer");

    let config = WriterConfig::from_env().context("invalid identity-chain-writer configuration")?;
    writer::run(config).await
}
