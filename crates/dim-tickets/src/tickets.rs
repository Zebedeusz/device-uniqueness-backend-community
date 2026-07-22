//! DIM ticket domain: the `(dim, network, status)` vocabulary, the Postgres
//! store, and the **frozen wire mapping** of the legacy route contract.
//!
//! Wire quirks preserved from the legacy backend (do not "fix"):
//! - Internal `SUBMITTING` is reported as `SUBMITTED`.
//! - A `PENDING` ticket's `updatedAt` is the **current server time**; every
//!   other status reports its state-change time (`updated_at ?? created_at`).
//! - `registered` is a deprecated boolean derived from the status.
//! - The create response echoes the address as both `ticket` and `who` and
//!   omits `onchainData` entirely; the status response always carries it
//!   (`null` unless registered).

use std::str::FromStr;

use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;

/// Which DIM (Decentralized Identity Module) a ticket funds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    /// The game personhood flow (`Game` pallet). Shipping priority.
    Game,
    /// The Proof-of-Ink flow (`ProofOfInk` pallet).
    ProofOfInk,
}

impl Dim {
    /// The frozen wire/DB literal.
    pub fn as_str(self) -> &'static str {
        match self {
            Dim::Game => "Game",
            Dim::ProofOfInk => "ProofOfInk",
        }
    }
}

impl FromStr for Dim {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Game" => Ok(Dim::Game),
            "ProofOfInk" => Ok(Dim::ProofOfInk),
            _ => Err(()),
        }
    }
}

/// Network literal stamped into rows and responses (frozen enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Westend next.
    Westend2,
    /// Paseo (default for dev/test).
    Paseo,
    /// Polkadot production.
    Polkadot,
}

impl Network {
    /// The frozen wire/DB literal.
    pub fn as_str(self) -> &'static str {
        match self {
            Network::Westend2 => "westend2",
            Network::Paseo => "paseo",
            Network::Polkadot => "polkadot",
        }
    }
}

impl FromStr for Network {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "westend2" => Ok(Network::Westend2),
            "paseo" => Ok(Network::Paseo),
            "polkadot" => Ok(Network::Polkadot),
            _ => Err(()),
        }
    }
}

/// A `dim_tickets` row as stored (statuses are the DB encodings, which include
/// the internal `SUBMITTING` the wire never sees).
#[derive(Debug, Clone)]
pub struct TicketRow {
    /// SS58 address the ticket is issued to (primary key).
    pub ticket: String,
    /// SS58 address of the funding inviter.
    pub inviter: String,
    /// Network literal.
    pub network: String,
    /// DIM literal.
    pub dim: String,
    /// DB status: `PENDING | SUBMITTING | SUBMITTED | REGISTERED | FAILED`.
    pub status: String,
    /// On-chain registration data (present once `REGISTERED`).
    pub onchain_data: Option<serde_json::Value>,
    /// Row creation time.
    pub created_at: OffsetDateTime,
    /// Last state-change time (`None` before the first transition).
    pub updated_at: Option<OffsetDateTime>,
}

impl TicketRow {
    fn from_pg(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            ticket: row.try_get("ticket")?,
            inviter: row.try_get("inviter")?,
            network: row.try_get("network")?,
            dim: row.try_get("dim")?,
            status: row.try_get("status")?,
            onchain_data: row.try_get("onchain_data")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// Fetch a ticket by its SS58 address.
pub async fn get_ticket(pool: &PgPool, ticket: &str) -> Result<Option<TicketRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT ticket, inviter, network, dim, status, onchain_data, created_at, updated_at \
         FROM dim_tickets WHERE ticket = $1",
    )
    .bind(ticket)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(TicketRow::from_pg).transpose()
}

/// Insert a fresh `PENDING` ticket; `None` means the address already has one
/// (the `ON CONFLICT DO NOTHING` lost — the caller maps that to `409`).
pub async fn insert_ticket(
    pool: &PgPool,
    ticket: &str,
    inviter: &str,
    network: Network,
    dim: Dim,
) -> Result<Option<TicketRow>, sqlx::Error> {
    let row = sqlx::query(
        "INSERT INTO dim_tickets (ticket, inviter, network, dim, status) \
         VALUES ($1, $2, $3, $4, 'PENDING') \
         ON CONFLICT (ticket) DO NOTHING \
         RETURNING ticket, inviter, network, dim, status, onchain_data, created_at, updated_at",
    )
    .bind(ticket)
    .bind(inviter)
    .bind(network.as_str())
    .bind(dim.as_str())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(TicketRow::from_pg).transpose()
}

/// A due row for the writer (raw DB encodings; the writer decodes and skips
/// malformed rows with a warning, as legacy did).
#[derive(Debug, Clone)]
pub struct DueRow {
    /// SS58 address (row key).
    pub ticket: String,
    /// DIM literal as stored.
    pub dim: String,
    /// Status as stored (`PENDING | SUBMITTING | SUBMITTED`).
    pub status: String,
    /// Failed submission attempts so far.
    pub retry_count: i32,
}

/// Fetch the writer's next batch: due `PENDING`/in-flight rows, FIFO.
pub async fn fetch_due(
    pool: &PgPool,
    now: OffsetDateTime,
    limit: i64,
    network: Network,
    inviter: &str,
) -> Result<Vec<DueRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT ticket, dim, status, retry_count FROM dim_tickets \
         WHERE status IN ('PENDING', 'SUBMITTING', 'SUBMITTED') \
           AND (retry_at IS NULL OR retry_at <= $1) \
           AND network = $3 AND inviter = $4 \
         ORDER BY created_at ASC \
         LIMIT $2",
    )
    .bind(now)
    .bind(limit)
    .bind(network.as_str())
    .bind(inviter)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(DueRow {
                ticket: row.try_get("ticket")?,
                dim: row.try_get("dim")?,
                status: row.try_get("status")?,
                retry_count: row.try_get("retry_count")?,
            })
        })
        .collect()
}

/// Mark retry-exhausted tickets terminally `FAILED`.
pub async fn mark_failed(
    pool: &PgPool,
    tickets: &[String],
    network: Network,
    inviter: &str,
) -> Result<(), sqlx::Error> {
    if tickets.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE dim_tickets SET status = 'FAILED', updated_at = now() \
         WHERE ticket = ANY($1) AND network = $2 AND inviter = $3",
    )
    .bind(tickets)
    .bind(network.as_str())
    .bind(inviter)
    .execute(pool)
    .await?;
    Ok(())
}

/// Reset orphaned in-flight tickets to `PENDING` (lease expired — a crashed or
/// errored earlier submission).
pub async fn recover_orphaned(
    pool: &PgPool,
    tickets: &[String],
    network: Network,
    inviter: &str,
) -> Result<(), sqlx::Error> {
    if tickets.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE dim_tickets SET status = 'PENDING', retry_at = NULL, updated_at = now() \
         WHERE ticket = ANY($1) AND network = $2 AND inviter = $3",
    )
    .bind(tickets)
    .bind(network.as_str())
    .bind(inviter)
    .execute(pool)
    .await?;
    Ok(())
}

/// Move a submission batch to `SUBMITTING` with its `retry_at` lease.
pub async fn mark_submitting(
    pool: &PgPool,
    tickets: &[String],
    retry_at: OffsetDateTime,
    network: Network,
    inviter: &str,
) -> Result<(), sqlx::Error> {
    if tickets.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE dim_tickets SET status = 'SUBMITTING', retry_at = $2, updated_at = now() \
         WHERE ticket = ANY($1) AND network = $3 AND inviter = $4",
    )
    .bind(tickets)
    .bind(retry_at)
    .bind(network.as_str())
    .bind(inviter)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark finalized tickets `REGISTERED` with their on-chain data.
///
/// Legacy quirk preserved: `updated_at` is NOT touched here, so a registered
/// ticket's wire `updatedAt` reports the submission time.
pub async fn mark_registered(
    pool: &PgPool,
    tickets: &[String],
    onchain_data: &serde_json::Value,
    network: Network,
    inviter: &str,
) -> Result<(), sqlx::Error> {
    if tickets.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE dim_tickets SET status = 'REGISTERED', onchain_data = $2 \
         WHERE ticket = ANY($1) AND network = $3 AND inviter = $4",
    )
    .bind(tickets)
    .bind(onchain_data)
    .bind(network.as_str())
    .bind(inviter)
    .execute(pool)
    .await?;
    Ok(())
}

/// Re-queue one failed batch item as `PENDING` with its incremented retry
/// count and backoff `retry_at`.
pub async fn schedule_retry(
    pool: &PgPool,
    ticket: &str,
    retry_count: i32,
    retry_at: OffsetDateTime,
    network: Network,
    inviter: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE dim_tickets \
         SET status = 'PENDING', retry_count = $2, retry_at = $3, updated_at = now() \
         WHERE ticket = $1 AND network = $4 AND inviter = $5",
    )
    .bind(ticket)
    .bind(retry_count)
    .bind(retry_at)
    .bind(network.as_str())
    .bind(inviter)
    .execute(pool)
    .await?;
    Ok(())
}

/// Format a timestamp exactly like JavaScript's `Date.toISOString()`
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`) — the shape the shipping clients parse.
pub fn to_iso_millis(ts: OffsetDateTime) -> String {
    let fmt = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    ts.to_offset(time::UtcOffset::UTC)
        .format(&fmt)
        .expect("static format description")
}

/// The frozen `GET /api/v1/dim-ticket/{who}` 200 body.
pub fn status_response(row: &TicketRow, now: OffsetDateTime) -> serde_json::Value {
    // SUBMITTING is internal; the wire only knows SUBMITTED.
    let wire_status = match row.status.as_str() {
        "SUBMITTING" => "SUBMITTED",
        other => other,
    };
    let state_ts = row.updated_at.unwrap_or(row.created_at);
    // Legacy quirk: PENDING reports "now", everything else its state change.
    let updated_at = match row.status.as_str() {
        "PENDING" => now,
        _ => state_ts,
    };
    let registered = row.status == "REGISTERED";
    let onchain_data = if registered {
        row.onchain_data.clone().unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    serde_json::json!({
        "ticket": row.ticket,
        "inviter": row.inviter,
        "network": row.network,
        "dim": row.dim,
        "status": wire_status,
        "registered": registered,
        "onchainData": onchain_data,
        "createdAt": to_iso_millis(row.created_at),
        "updatedAt": to_iso_millis(updated_at),
    })
}

/// The frozen `POST /api/v1/dim-ticket` 200 body (no `onchainData` key; the
/// address echoes as both `ticket` and `who`).
pub fn create_response(row: &TicketRow, now: OffsetDateTime) -> serde_json::Value {
    serde_json::json!({
        "ticket": row.ticket,
        "who": row.ticket,
        "inviter": row.inviter,
        "network": row.network,
        "dim": row.dim,
        "status": "PENDING",
        "registered": false,
        "createdAt": to_iso_millis(row.created_at),
        "updatedAt": to_iso_millis(now),
    })
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn row(status: &str) -> TicketRow {
        TicketRow {
            ticket: "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty".to_string(),
            inviter: "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM".to_string(),
            network: "paseo".to_string(),
            dim: "Game".to_string(),
            status: status.to_string(),
            onchain_data: None,
            created_at: datetime!(2026-07-16 12:00:00.000 UTC),
            updated_at: Some(datetime!(2026-07-16 12:05:00.000 UTC)),
        }
    }

    const NOW: OffsetDateTime = datetime!(2026-07-16 13:00:00.500 UTC);

    #[test]
    fn formats_timestamps_like_to_iso_string() {
        assert_eq!(to_iso_millis(NOW), "2026-07-16T13:00:00.500Z");
        assert_eq!(
            to_iso_millis(datetime!(2026-01-02 03:04:05 UTC)),
            "2026-01-02T03:04:05.000Z"
        );
    }

    #[test]
    fn submitting_is_reported_as_submitted() {
        let body = status_response(&row("SUBMITTING"), NOW);
        assert_eq!(body["status"], "SUBMITTED");
        assert_eq!(body["registered"], false);
        assert_eq!(body["updatedAt"], "2026-07-16T12:05:00.000Z");
    }

    #[test]
    fn pending_reports_current_time_as_updated_at() {
        let body = status_response(&row("PENDING"), NOW);
        assert_eq!(body["status"], "PENDING");
        assert_eq!(body["updatedAt"], "2026-07-16T13:00:00.500Z");
        assert_eq!(body["onchainData"], serde_json::Value::Null);
    }

    #[test]
    fn registered_derives_flag_and_carries_onchain_data() {
        let mut r = row("REGISTERED");
        r.onchain_data = Some(serde_json::json!({ "blockHash": "0xabc", "blockNumber": 7 }));
        let body = status_response(&r, NOW);
        assert_eq!(body["status"], "REGISTERED");
        assert_eq!(body["registered"], true);
        assert_eq!(body["onchainData"]["blockNumber"], 7);
        assert_eq!(body["updatedAt"], "2026-07-16T12:05:00.000Z");
    }

    #[test]
    fn failed_reports_state_change_time() {
        let mut r = row("FAILED");
        r.updated_at = None; // never transitioned cleanly — falls back to created_at
        let body = status_response(&r, NOW);
        assert_eq!(body["status"], "FAILED");
        assert_eq!(body["updatedAt"], "2026-07-16T12:00:00.000Z");
    }

    #[test]
    fn create_response_echoes_who_and_omits_onchain_data() {
        let body = create_response(&row("PENDING"), NOW);
        assert_eq!(body["ticket"], body["who"]);
        assert_eq!(body["status"], "PENDING");
        assert_eq!(body["registered"], false);
        assert!(body.get("onchainData").is_none());
        assert_eq!(body["createdAt"], "2026-07-16T12:00:00.000Z");
        assert_eq!(body["updatedAt"], "2026-07-16T13:00:00.500Z");
    }
}
