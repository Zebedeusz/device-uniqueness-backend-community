//! Validated prefix search, stable cursors, and mobile wire response types.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;

const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 1000;

/// Query parameters accepted by the public search endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    /// Case-insensitive username prefix, from 1 through 64 characters.
    pub prefix: String,
    /// Optional compatibility filter; only `ASSIGNED` is accepted.
    pub status: Option<String>,
    /// Requested page size, defaulting to 100 and capped at 1,000.
    pub limit: Option<u32>,
    /// Opaque continuation cursor from an earlier response.
    pub cursor: Option<String>,
    /// Compatibility flag; indexed rows have no truthful event position.
    pub include_onchain_data: Option<bool>,
}

/// One assigned username in the shipping mobile response shape.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchUsername {
    /// People Chain account encoded as SS58.
    pub account_id: String,
    /// Full username when present, otherwise the lite username.
    pub username: String,
    /// Assignment state, always `ASSIGNED` for this projection.
    pub status: &'static str,
    /// On-chain event position, always null because the snapshot has none.
    pub onchain_data: Option<serde_json::Value>,
    /// Time the projection first observed the account.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Time the projection last refreshed the account.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Public search response envelope.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// Assigned usernames in deterministic continuation order.
    pub usernames: Vec<SearchUsername>,
    /// Opaque cursor for the next page, or null when exhausted.
    pub next_cursor: Option<String>,
}

/// Search validation, cursor, or database failure.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Query validation failed.
    #[error("{0}")]
    InvalidQuery(String),
    /// Cursor decoding or validation failed.
    #[error("invalid cursor")]
    InvalidCursor,
    /// Database query failed.
    #[error("searching username projection: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    display_key: String,
    lite_base: String,
    lite_digits: String,
    account_id: [u8; 32],
}

struct SearchRow {
    item: SearchUsername,
    cursor: Cursor,
}

/// Search assigned usernames using a stable, deterministic continuation tuple.
pub async fn search(pool: &PgPool, query: SearchQuery) -> Result<SearchResponse, SearchError> {
    let validated = validate(query)?;
    let cursor = validated.cursor.as_deref().map(decode_cursor).transpose()?;
    let pattern = format!("{}%", escape_like(&validated.prefix.to_lowercase()));
    let fetch_limit = i64::from(validated.limit) + 1;

    let rows = sqlx::query(
        "SELECT account_id, account_id_ss58, display_username,
                lower(display_username) COLLATE \"C\" AS display_key,
                lite_base, lite_digits::text AS lite_digits_text,
                created_at, updated_at
         FROM assigned_usernames
         WHERE lower(display_username) COLLATE \"C\" LIKE $1 ESCAPE '\\'
           AND (
             $2::text IS NULL OR
             ROW(
               lower(display_username) COLLATE \"C\",
               lite_base COLLATE \"C\",
               lite_digits,
               account_id
             ) > ROW(
               $2::text COLLATE \"C\",
               $3::text COLLATE \"C\",
               $4::numeric,
               $5::bytea
             )
           )
         ORDER BY lower(display_username) COLLATE \"C\",
                  lite_base COLLATE \"C\",
                  lite_digits,
                  account_id
         LIMIT $6",
    )
    .bind(pattern)
    .bind(cursor.as_ref().map(|value| value.display_key.as_str()))
    .bind(cursor.as_ref().map(|value| value.lite_base.as_str()))
    .bind(cursor.as_ref().map(|value| value.lite_digits.as_str()))
    .bind(cursor.as_ref().map(|value| value.account_id.as_slice()))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await?;

    let decoded = rows
        .into_iter()
        .map(row_from_db)
        .collect::<Result<Vec<_>, _>>()?;
    let (usernames, next_cursor) = assemble_page(decoded, validated.limit as usize);

    Ok(SearchResponse {
        usernames,
        next_cursor,
    })
}

/// Turn a `limit + 1` fetch into a page plus an optional continuation cursor.
///
/// `search` deliberately queries `LIMIT limit + 1`; this pure function encodes
/// the paging rule so the tested code is the real code: when an extra row is
/// present the page is full, so the extra row is dropped and the cursor of the
/// last kept row is encoded. The next page's SQL resumes strictly after that
/// tuple, so pages never overlap or repeat a row. Fewer than `limit + 1` rows
/// means the results are exhausted and no cursor is returned.
fn assemble_page(mut rows: Vec<SearchRow>, limit: usize) -> (Vec<SearchUsername>, Option<String>) {
    let has_more = rows.len() > limit;
    if has_more {
        rows.pop();
    }
    let next_cursor = if has_more {
        rows.last().map(|row| encode_cursor(&row.cursor))
    } else {
        None
    };
    let usernames = rows.into_iter().map(|row| row.item).collect();
    (usernames, next_cursor)
}

struct ValidatedQuery {
    prefix: String,
    limit: u32,
    cursor: Option<String>,
}

fn validate(query: SearchQuery) -> Result<ValidatedQuery, SearchError> {
    let prefix_len = query.prefix.chars().count();
    if !(1..=64).contains(&prefix_len) {
        return Err(SearchError::InvalidQuery(
            "prefix must contain between 1 and 64 characters".to_string(),
        ));
    }
    if query
        .status
        .as_deref()
        .is_some_and(|status| status != "ASSIGNED")
    {
        return Err(SearchError::InvalidQuery(
            "status must be ASSIGNED when provided".to_string(),
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(SearchError::InvalidQuery(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    let _ = query.include_onchain_data;
    Ok(ValidatedQuery {
        prefix: query.prefix,
        limit,
        cursor: query.cursor,
    })
}

fn row_from_db(row: sqlx::postgres::PgRow) -> Result<SearchRow, SearchError> {
    let account_id = row.try_get::<Vec<u8>, _>("account_id")?;
    let account_id: [u8; 32] = account_id.try_into().map_err(|_| {
        SearchError::Database(sqlx::Error::ColumnDecode {
            index: "account_id".to_string(),
            source: "expected 32-byte account ID".into(),
        })
    })?;
    let display_key = row.try_get("display_key")?;
    let lite_base = row.try_get("lite_base")?;
    let lite_digits = row.try_get("lite_digits_text")?;

    Ok(SearchRow {
        item: SearchUsername {
            account_id: row.try_get("account_id_ss58")?,
            username: row.try_get("display_username")?,
            status: "ASSIGNED",
            onchain_data: None,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        },
        cursor: Cursor {
            display_key,
            lite_base,
            lite_digits,
            account_id,
        },
    })
}

fn encode_cursor(cursor: &Cursor) -> String {
    let bytes = serde_json::to_vec(cursor).expect("cursor serialization cannot fail");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_cursor(encoded: &str) -> Result<Cursor, SearchError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| SearchError::InvalidCursor)?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| SearchError::InvalidCursor)?;
    if cursor.display_key.is_empty()
        || cursor.lite_base.is_empty()
        || cursor.lite_digits.is_empty()
        || !cursor.lite_digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(SearchError::InvalidCursor);
    }
    Ok(cursor)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use axum::extract::Query;
    use axum::http::Uri;
    use time::macros::datetime;

    use super::{assemble_page, decode_cursor, encode_cursor, validate};
    use super::{Cursor, SearchError, SearchQuery, SearchResponse, SearchRow, SearchUsername};

    fn sample_row(index: u8) -> SearchRow {
        SearchRow {
            item: SearchUsername {
                account_id: format!("5Account{index}"),
                username: format!("alice.{index}"),
                status: "ASSIGNED",
                onchain_data: None,
                created_at: datetime!(2026-07-11 10:00 UTC),
                updated_at: datetime!(2026-07-11 10:01 UTC),
            },
            cursor: Cursor {
                display_key: format!("alice.{index}"),
                lite_base: "alice".to_string(),
                lite_digits: index.to_string(),
                account_id: [index; 32],
            },
        }
    }

    #[test]
    fn cursor_roundtrips_and_rejects_invalid_input() {
        let cursor = Cursor {
            display_key: "alice.12".to_string(),
            lite_base: "alice".to_string(),
            lite_digits: "12".to_string(),
            account_id: [7; 32],
        };
        assert_eq!(
            decode_cursor(&encode_cursor(&cursor)).expect("cursor"),
            cursor
        );
        assert!(matches!(
            decode_cursor("not-a-cursor"),
            Err(SearchError::InvalidCursor)
        ));
    }

    #[test]
    fn accepts_ios_and_android_wire_queries() {
        let ios_uri: Uri = "/api/v1/usernames/search?prefix=ali"
            .parse()
            .expect("iOS URI");
        let Query(ios_query) = Query::<SearchQuery>::try_from_uri(&ios_uri).expect("iOS query");
        let ios = validate(ios_query).expect("valid iOS query");
        assert_eq!(ios.limit, 100);

        let android_uri: Uri = "/api/v1/usernames/search?status=ASSIGNED&prefix=ali"
            .parse()
            .expect("Android URI");
        let Query(android_query) =
            Query::<SearchQuery>::try_from_uri(&android_uri).expect("Android query");
        validate(android_query).expect("valid Android query");

        let android_full_uri: Uri =
            "/api/v1/usernames/search?status=ASSIGNED&prefix=ali&includeOnchainData=true"
                .parse()
                .expect("Android URI with onchain flag");
        let Query(android_full_query) =
            Query::<SearchQuery>::try_from_uri(&android_full_uri).expect("Android query");
        validate(android_full_query).expect("valid Android query with onchain flag");
    }

    #[test]
    fn rejects_invalid_prefix_status_and_limit() {
        for query in [
            SearchQuery {
                prefix: String::new(),
                ..SearchQuery::default()
            },
            SearchQuery {
                prefix: "a".repeat(65),
                ..SearchQuery::default()
            },
            SearchQuery {
                prefix: "a".to_string(),
                status: Some("RESERVED".to_string()),
                ..SearchQuery::default()
            },
            SearchQuery {
                prefix: "a".to_string(),
                limit: Some(0),
                ..SearchQuery::default()
            },
        ] {
            assert!(matches!(validate(query), Err(SearchError::InvalidQuery(_))));
        }
    }

    #[test]
    fn golden_non_empty_response_matches_mobile_wire_shape() {
        let response = SearchResponse {
            usernames: vec![SearchUsername {
                account_id: "5Example".to_string(),
                username: "alice.12".to_string(),
                status: "ASSIGNED",
                onchain_data: None,
                created_at: datetime!(2026-07-11 10:00 UTC),
                updated_at: datetime!(2026-07-11 10:01 UTC),
            }],
            next_cursor: Some("eyJvcGFxdWUiOiJjdXJzb3IifQ".to_string()),
        };
        let value = serde_json::to_value(response).expect("serialize response");
        assert_eq!(
            value,
            serde_json::json!({
                "usernames": [{
                    "accountId": "5Example",
                    "username": "alice.12",
                    "status": "ASSIGNED",
                    "onchainData": null,
                    "createdAt": "2026-07-11T10:00:00Z",
                    "updatedAt": "2026-07-11T10:01:00Z"
                }],
                "nextCursor": "eyJvcGFxdWUiOiJjdXJzb3IifQ"
            })
        );
    }

    #[test]
    fn golden_empty_response_matches_mobile_wire_shape() {
        let response = SearchResponse {
            usernames: vec![],
            next_cursor: None,
        };
        let value = serde_json::to_value(response).expect("serialize response");
        assert_eq!(
            value,
            serde_json::json!({
                "usernames": [],
                "nextCursor": null
            })
        );
    }

    #[test]
    fn assemble_page_returns_cursor_of_last_kept_row_when_more_remain() {
        let limit = 2usize;
        let rows = vec![sample_row(1), sample_row(2), sample_row(3)];
        let (usernames, next_cursor) = assemble_page(rows, limit);

        assert_eq!(usernames.len(), limit);
        assert_eq!(usernames[0].username, "alice.1");
        assert_eq!(usernames[1].username, "alice.2");

        let encoded = next_cursor.expect("cursor when more rows remain");
        let decoded = decode_cursor(&encoded).expect("valid continuation cursor");
        assert_eq!(decoded, sample_row(2).cursor);
    }

    #[test]
    fn assemble_page_returns_no_cursor_when_page_not_full() {
        let (usernames, next_cursor) = assemble_page(vec![sample_row(1)], 2);
        assert_eq!(usernames.len(), 1);
        assert!(next_cursor.is_none());
    }

    #[test]
    fn assemble_page_handles_empty_rows() {
        let (usernames, next_cursor) = assemble_page(Vec::new(), 2);
        assert!(usernames.is_empty());
        assert!(next_cursor.is_none());
    }
}
