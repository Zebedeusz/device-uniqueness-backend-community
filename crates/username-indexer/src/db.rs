//! Service-owned Postgres connection and migrations.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Database startup failure.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// Postgres connection failed.
    #[error("connecting to Postgres: {0}")]
    Connect(#[source] sqlx::Error),
    /// Embedded migrations failed.
    #[error("running database migrations: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),
}

/// Connect to the service database and apply embedded migrations.
pub async fn connect(database_url: &str) -> Result<PgPool, DbError> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
        .map_err(DbError::Connect)?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(DbError::Migrate)?;

    Ok(pool)
}
