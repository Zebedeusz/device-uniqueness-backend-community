//! `registration-queue` — the standalone single-instance queue-advancer
//! service. Holds the advancer lease (which doubles as the queue's liveness
//! signal) and drains the registration queue by the spec's slot rules. If
//! this process is down, `identity-api` and `identity-chain-writer` fall back
//! to the original direct registration path on their own — see
//! [`identity_service::queue`].

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

use identity_service::queue::{self, AdvancerConfig};
use identity_service::ChainClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = AdvancerConfig::from_env().context("invalid registration-queue configuration")?;
    let pool = identity_service::db::connect(&config.database_url).await?;
    let chain = ChainClient::connect(&config.people_rpc_url).await?;
    queue::run_advancer(pool, chain, config).await;
    Ok(())
}
