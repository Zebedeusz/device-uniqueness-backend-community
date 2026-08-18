//! `device-attestation-chain-writer` — single-instance worker role. Loads [`WriterConfig`] and
//! drains the reservation outbox onto People Chain. See [`device_attestation::chain::writer`].

use anyhow::Context as _;

use device_attestation::chain::writer::{self, WriterConfig};

pub async fn run() -> anyhow::Result<()> {
    http_common::telemetry::init("device-attestation-chain-writer");
    http_common::metrics::spawn("device-attestation-chain-writer");

    let config = WriterConfig::from_env()
        .context("invalid device-attestation-chain-writer configuration")?;
    writer::run(config).await
}
