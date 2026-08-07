//! Chain access and the username write path.
//!
//! Groups the People read/submit RPC [`client`], the [`asset_hub`] client
//! behind the dotNS gateway lane, and the reservation [`outbox`]. The
//! single-instance [`writer`] worker drains that outbox onto both chains.

pub mod asset_hub;
pub mod client;
pub mod lease;
pub mod outbox;
pub mod writer;

pub use client::ChainClient;
