//! Reconnecting People Chain client and finalized storage snapshot access.

use std::sync::Arc;

use chain_types::PeopleConfig;
use subxt::backend::LegacyBackend;
use subxt::OnlineClient;
use subxt_rpcs::client::{ReconnectingRpcClient, RpcClient};

/// Boxed chain transport or metadata error.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// People Chain connection or query failure.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    /// RPC transport setup failed.
    #[error("connecting to People Chain at {url}: {source}")]
    Connect {
        /// Configured endpoint.
        url: String,
        /// Underlying transport error.
        #[source]
        source: BoxError,
    },
    /// Online client metadata initialization failed.
    #[error("initializing People Chain client: {0}")]
    Initialize(#[source] BoxError),
    /// A finalized snapshot query failed.
    #[error("querying People Chain finalized snapshot: {0}")]
    Query(#[source] BoxError),
}

/// Connected People Chain client using a reconnecting legacy backend.
#[derive(Clone)]
pub struct ChainClient {
    client: OnlineClient<PeopleConfig>,
}

impl ChainClient {
    /// Connect and configure bounded legacy storage paging.
    pub async fn connect(url: &str, storage_page_size: u32) -> Result<Self, ChainError> {
        let reconnecting = ReconnectingRpcClient::builder()
            .build(url)
            .await
            .map_err(|source| ChainError::Connect {
                url: url.to_string(),
                source: Box::new(source),
            })?;
        let backend = LegacyBackend::<PeopleConfig>::builder()
            .storage_page_size(storage_page_size)
            .build(RpcClient::new(reconnecting));
        let client = OnlineClient::from_backend(Arc::new(backend))
            .await
            .map_err(|source| ChainError::Initialize(Box::new(source)))?;
        Ok(Self { client })
    }

    /// Return the underlying online client.
    pub fn online(&self) -> &OnlineClient<PeopleConfig> {
        &self.client
    }

    /// Verify that a current finalized block is reachable.
    pub async fn health(&self) -> Result<(), ChainError> {
        self.client
            .at_current_block()
            .await
            .map_err(|source| ChainError::Query(Box::new(source)))?;
        Ok(())
    }
}
