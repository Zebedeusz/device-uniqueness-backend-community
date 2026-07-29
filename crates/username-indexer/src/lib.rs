//! Independent finalized People Chain username projection and search service.

pub mod bootstrap;
pub mod chain;
pub mod config;
pub mod db;
pub mod http;
pub mod incremental;
pub mod openapi;
pub mod poc;
pub mod projection;
pub mod search;
pub mod ss58;
pub mod sync;

pub use bootstrap::{ensure_seeded, BootstrapError, BootstrapReport};
pub use chain::{ChainClient, ChainError};
pub use config::{Config, ConfigError};
pub use http::{routes, AppState};
pub use incremental::{index_finalized_range, IndexError, IndexReport};
pub use poc::{Poc, Puzzle, Solution};
pub use projection::AssignedUsername;
pub use sync::{run as run_sync, Freshness, FreshnessSnapshot};
