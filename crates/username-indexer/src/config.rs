//! Runtime configuration for username-indexer.

use std::net::SocketAddr;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_STORAGE_PAGE_SIZE: &str = "1000";
const MAX_STORAGE_PAGE_SIZE: u32 = 1_000;
const DEFAULT_SYNC_INTERVAL_SECS: &str = "30";
const MAX_SYNC_INTERVAL_SECS: u32 = 86_400;
const DEFAULT_SEARCH_RATE_LIMIT: &str = "60";
const DEFAULT_SEARCH_RATE_LIMIT_WINDOW_SECS: &str = "60";

/// Validated username-indexer runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Service-owned Postgres connection string.
    pub database_url: String,
    /// People Chain WebSocket RPC endpoint.
    pub people_rpc_url: String,
    /// Maximum number of storage entries requested and written per page.
    pub storage_page_size: u32,
    /// Poll interval, in seconds, for new finalized blocks.
    pub sync_interval_secs: u32,
    /// Requests per window per client IP on the public search route.
    pub search_rate_limit: u32,
    /// Rate-limit window, in seconds, for the public search route.
    pub search_rate_limit_window_secs: u32,
}

/// A missing or malformed configuration value.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A required environment variable was not set or was empty.
    #[error("required environment variable {0} is not set")]
    Missing(&'static str),
    /// An environment variable held an invalid value.
    #[error("environment variable {key} is invalid: {reason}")]
    Invalid {
        /// The offending variable name.
        key: &'static str,
        /// Why the value is invalid.
        reason: String,
    },
}

impl Config {
    /// Read and validate configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_getter(|key| std::env::var(key).ok())
    }

    /// Read configuration through a caller-provided lookup function.
    pub fn from_getter<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let database_url = required(&get, "DATABASE_URL")?;
        let people_rpc_url = required(&get, "PEOPLE_RPC_URL")?;
        let bind_addr = parse(
            "BIND_ADDR",
            get("BIND_ADDR").as_deref().unwrap_or(DEFAULT_BIND_ADDR),
        )?;
        let storage_page_size = parse(
            "STORAGE_PAGE_SIZE",
            get("STORAGE_PAGE_SIZE")
                .as_deref()
                .unwrap_or(DEFAULT_STORAGE_PAGE_SIZE),
        )?;
        if !(1..=MAX_STORAGE_PAGE_SIZE).contains(&storage_page_size) {
            return Err(ConfigError::Invalid {
                key: "STORAGE_PAGE_SIZE",
                reason: format!("must be between 1 and {MAX_STORAGE_PAGE_SIZE}"),
            });
        }
        let sync_interval_secs = parse(
            "SYNC_INTERVAL_SECS",
            get("SYNC_INTERVAL_SECS")
                .as_deref()
                .unwrap_or(DEFAULT_SYNC_INTERVAL_SECS),
        )?;
        if !(1..=MAX_SYNC_INTERVAL_SECS).contains(&sync_interval_secs) {
            return Err(ConfigError::Invalid {
                key: "SYNC_INTERVAL_SECS",
                reason: format!("must be between 1 and {MAX_SYNC_INTERVAL_SECS}"),
            });
        }
        let search_rate_limit = parse(
            "SEARCH_RATE_LIMIT",
            get("SEARCH_RATE_LIMIT")
                .as_deref()
                .unwrap_or(DEFAULT_SEARCH_RATE_LIMIT),
        )?;
        if search_rate_limit == 0 {
            return Err(ConfigError::Invalid {
                key: "SEARCH_RATE_LIMIT",
                reason: "must be at least 1".to_string(),
            });
        }
        let search_rate_limit_window_secs = parse(
            "SEARCH_RATE_LIMIT_WINDOW_SECS",
            get("SEARCH_RATE_LIMIT_WINDOW_SECS")
                .as_deref()
                .unwrap_or(DEFAULT_SEARCH_RATE_LIMIT_WINDOW_SECS),
        )?;
        if search_rate_limit_window_secs == 0 {
            return Err(ConfigError::Invalid {
                key: "SEARCH_RATE_LIMIT_WINDOW_SECS",
                reason: "must be at least 1".to_string(),
            });
        }

        Ok(Self {
            bind_addr,
            database_url,
            people_rpc_url,
            storage_page_size,
            sync_interval_secs,
            search_rate_limit,
            search_rate_limit_window_secs,
        })
    }
}

fn required<F>(get: &F, key: &'static str) -> Result<String, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    get(key)
        .filter(|value| !value.trim().is_empty())
        .ok_or(ConfigError::Missing(key))
}

fn parse<T>(key: &'static str, raw: &str) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    raw.parse().map_err(|error: T::Err| ConfigError::Invalid {
        key,
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Config, ConfigError};

    fn config(values: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        Config::from_getter(|key| values.get(key).cloned())
    }

    #[test]
    fn requires_database_and_people_rpc_urls() {
        assert_eq!(config(&[]), Err(ConfigError::Missing("DATABASE_URL")));
        assert_eq!(
            config(&[("DATABASE_URL", "postgres://localhost/read")]),
            Err(ConfigError::Missing("PEOPLE_RPC_URL"))
        );
    }

    #[test]
    fn applies_defaults_and_accepts_overrides() {
        let defaults = config(&[
            ("DATABASE_URL", "postgres://localhost/read"),
            ("PEOPLE_RPC_URL", "ws://localhost:9944"),
        ])
        .expect("valid defaults");
        assert_eq!(defaults.bind_addr.to_string(), "0.0.0.0:8080");
        assert_eq!(defaults.storage_page_size, 1000);
        assert_eq!(defaults.sync_interval_secs, 30);
        assert_eq!(defaults.search_rate_limit, 60);
        assert_eq!(defaults.search_rate_limit_window_secs, 60);

        let overridden = config(&[
            ("DATABASE_URL", "postgres://localhost/read"),
            ("PEOPLE_RPC_URL", "ws://localhost:9944"),
            ("BIND_ADDR", "127.0.0.1:9000"),
            ("STORAGE_PAGE_SIZE", "250"),
            ("SYNC_INTERVAL_SECS", "5"),
            ("SEARCH_RATE_LIMIT", "10"),
            ("SEARCH_RATE_LIMIT_WINDOW_SECS", "15"),
        ])
        .expect("valid overrides");
        assert_eq!(overridden.bind_addr.to_string(), "127.0.0.1:9000");
        assert_eq!(overridden.storage_page_size, 250);
        assert_eq!(overridden.sync_interval_secs, 5);
        assert_eq!(overridden.search_rate_limit, 10);
        assert_eq!(overridden.search_rate_limit_window_secs, 15);
    }

    #[test]
    fn rejects_zero_and_excessive_page_sizes() {
        for value in ["0", "1001"] {
            let error = config(&[
                ("DATABASE_URL", "postgres://localhost/read"),
                ("PEOPLE_RPC_URL", "ws://localhost:9944"),
                ("STORAGE_PAGE_SIZE", value),
            ])
            .expect_err("invalid page size");
            assert!(matches!(
                error,
                ConfigError::Invalid {
                    key: "STORAGE_PAGE_SIZE",
                    ..
                }
            ));
        }
    }

    #[test]
    fn rejects_zero_and_excessive_sync_intervals() {
        for value in ["0", "86401"] {
            let error = config(&[
                ("DATABASE_URL", "postgres://localhost/read"),
                ("PEOPLE_RPC_URL", "ws://localhost:9944"),
                ("SYNC_INTERVAL_SECS", value),
            ])
            .expect_err("invalid sync interval");
            assert!(matches!(
                error,
                ConfigError::Invalid {
                    key: "SYNC_INTERVAL_SECS",
                    ..
                }
            ));
        }
    }

    #[test]
    fn rejects_zero_search_rate_limit() {
        let error = config(&[
            ("DATABASE_URL", "postgres://localhost/read"),
            ("PEOPLE_RPC_URL", "ws://localhost:9944"),
            ("SEARCH_RATE_LIMIT", "0"),
        ])
        .expect_err("invalid search rate limit");
        assert!(matches!(
            error,
            ConfigError::Invalid {
                key: "SEARCH_RATE_LIMIT",
                ..
            }
        ));
    }
}
