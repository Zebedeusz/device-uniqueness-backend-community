//! People Chain RPC client (subxt online).
//!
//! Backs the username availability reads (`Resources::UsernameOwnerOf`) and
//! exposes the underlying online client ([`ChainClient::online`]) that the
//! identity-chain-writer submits registrations through. On-device availability and the
//! standalone indexer are later work. Lite usernames are stored as `{base}.{NN}`
//! (two-digit discriminator `00`..`99`), so availability for a base is "which of
//! its 100 discriminators are unowned".

use std::collections::BTreeSet;

use anyhow::Context as _;
use chain_types::people;
use chain_types::PeopleConfig;
use futures::future::try_join_all;
use subxt::OnlineClient;
use subxt_rpcs::client::{ReconnectingRpcClient, RpcClient};

/// Number of discriminators per base (`00`..`99`).
const DISCRIMINATORS: u8 = 100;

/// The `Resources::UsernameOwnerOf` map key for a username's raw bytes.
fn owner_key(
    username: impl AsRef<[u8]>,
) -> people::runtime_types::bounded_collections::bounded_vec::BoundedVec<u8> {
    people::runtime_types::bounded_collections::bounded_vec::BoundedVec(username.as_ref().to_vec())
}

/// Connected People Chain client (cheap to clone; shares one connection).
#[derive(Clone)]
pub struct ChainClient {
    client: OnlineClient<PeopleConfig>,
}

impl ChainClient {
    /// Connect to the People Chain RPC and load its metadata.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client =
            OnlineClient::<PeopleConfig>::from_rpc_client(reconnecting_rpc_client(url).await?)
                .await
                .with_context(|| format!("connecting to People Chain at {url}"))?;
        Ok(Self { client })
    }

    /// Liveness probe used by `/readyz`.
    pub async fn health(&self) -> anyhow::Result<()> {
        self.client
            .at_current_block()
            .await
            .context("People Chain unreachable")?;
        Ok(())
    }

    /// The underlying online client, for the identity-chain-writer's tx submission.
    pub fn online(&self) -> &OnlineClient<PeopleConfig> {
        &self.client
    }

    /// The current owner of a full lite username (`base.digits`), if any.
    ///
    /// Used by the identity-chain-writer to reconcile: an unknown submit outcome is
    /// resolved by reading `UsernameOwnerOf` rather than blindly resubmitting.
    pub async fn username_owner(&self, full_username: &str) -> anyhow::Result<Option<[u8; 32]>> {
        let at = self.client.at_current_block().await?;
        let address = people::storage().resources().username_owner_of();
        let owner = at
            .storage()
            .try_fetch(address, (owner_key(full_username),))
            .await?;
        match owner {
            Some(value) => Ok(Some(value.decode()?.0)),
            None => Ok(None),
        }
    }

    /// Return the set of discriminators (`0`..`99`) already owned for `base`.
    ///
    /// Reads all 100 `{base}.{NN}` keys against one block snapshot.
    pub async fn taken_discriminators(&self, base: &str) -> anyhow::Result<BTreeSet<u8>> {
        let at = self.client.at_current_block().await?;
        let storage = at.storage();
        let storage = &storage;

        let lookups = (0..DISCRIMINATORS).map(move |discriminator| async move {
            let username = format!("{base}.{discriminator:02}");
            let address = people::storage().resources().username_owner_of();
            let owned = storage
                .try_fetch(address, (owner_key(username),))
                .await?
                .is_some();
            anyhow::Ok((discriminator, owned))
        });

        let results = try_join_all(lookups).await?;
        Ok(results
            .into_iter()
            .filter_map(|(discriminator, owned)| owned.then_some(discriminator))
            .collect())
    }
}

async fn reconnecting_rpc_client(url: &str) -> anyhow::Result<RpcClient> {
    let rpc_client = ReconnectingRpcClient::builder()
        .build(url)
        .await
        .with_context(|| format!("connecting to People Chain at {url}"))?;
    Ok(RpcClient::new(rpc_client))
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    use anyhow::Context as _;
    use jsonrpsee_core::server::RpcModule;
    use jsonrpsee_server::{ServerBuilder, ServerHandle};
    use subxt_rpcs::rpc_params;

    use super::reconnecting_rpc_client;

    #[tokio::test]
    async fn reconnecting_rpc_client_recovers_after_server_restart() -> anyhow::Result<()> {
        let (server, url) = TestRpcServer::start(None).await?;
        let rpc = reconnecting_rpc_client(&url).await?;

        let first: String = rpc.request("ping", rpc_params![]).await?;
        assert_eq!(first, "pong");

        server.stop().await?;
        let addr = url
            .strip_prefix("ws://")
            .context("test RPC URL should be ws://")?
            .parse::<SocketAddr>()?;
        let (_server, _) = TestRpcServer::start(Some(addr)).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let second = tokio::time::timeout(
            Duration::from_secs(5),
            rpc.request::<String>("ping", rpc_params![]),
        )
        .await??;
        assert_eq!(second, "pong");

        Ok(())
    }

    struct TestRpcServer {
        handle: ServerHandle,
    }

    impl TestRpcServer {
        async fn start(addr: Option<SocketAddr>) -> anyhow::Result<(Self, String)> {
            let addr = addr.unwrap_or_else(|| SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into());
            let server = ServerBuilder::default().build(addr).await?;
            let addr = server.local_addr()?;
            let mut module = RpcModule::new(());
            module.register_async_method("ping", |_, _, _| async { "pong" })?;
            let handle = server.start(module);

            Ok((Self { handle }, format!("ws://{addr}")))
        }

        async fn stop(self) -> anyhow::Result<()> {
            self.handle.stop()?;
            self.handle.stopped().await;
            Ok(())
        }
    }
}
