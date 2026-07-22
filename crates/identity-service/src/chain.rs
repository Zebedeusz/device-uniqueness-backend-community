//! People Chain access and the username write path.
//!
//! Groups the read/submit RPC [`client`], the reservation [`outbox`], and the
//! single-instance [`writer`] worker that drains the outbox onto chain.

pub mod client;
pub mod outbox;
pub mod writer;

pub use client::ChainClient;
