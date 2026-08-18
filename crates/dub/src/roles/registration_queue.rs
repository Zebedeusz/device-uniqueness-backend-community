//! `registration-queue` — the standalone single-instance queue-advancer
//! role. Holds the advancer lease (which doubles as the queue's liveness
//! signal) and drains the registration queue by the spec's slot rules. While
//! `QUEUE_ENABLED` is on, this role's promotion is the only way a claim
//! leaves the queue — if the process is down, claims park as `QUEUED` behind
//! the free lane's throttle and the chain-writer raises a stranded-queue
//! warning until it is restarted — see [`device_attestation::queue`].

use anyhow::Context as _;

use device_attestation::queue::{self, AdvancerConfig};
use device_attestation::ChainClient;

pub async fn run() -> anyhow::Result<()> {
    http_common::telemetry::init("registration-queue");
    http_common::metrics::spawn("registration-queue");

    let config = AdvancerConfig::from_env().context("invalid registration-queue configuration")?;
    let pool = device_attestation::db::connect(&config.database_url).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    queue::run_advancer(pool, chain, config).await;
    Ok(())
}
