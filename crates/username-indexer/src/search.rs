//! Validated prefix search, stable cursors, and the response shapes.
//!
//! Validation collects every query failure as a `FieldError` in field order
//! (`prefix`, then `limit`) rather than stopping at the first, `limit` is
//! **clamped** to 1,000 rather than rejected for being large, unknown query
//! parameters are ignored, and an invalid cursor short-circuits everything.

use std::collections::HashMap;

use base64::Engine as _;
use http_common::FieldError;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row as _};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 1000;

/// Rejection messages for the `prefix` query parameter.
const PREFIX_REQUIRED: &str = "Prefix is required";
const PREFIX_TOO_LONG: &str = "Prefix must be at most 64 characters";
const PREFIX_PATTERN: &str = "Prefix must be letters/digits, optionally followed by a dot and digits (e.g. \"alice\", \"alice.\", \"alice.10\")";

/// RFC 3339 timestamps in UTC (`2026-01-01T00:00:00Z`).
pub(crate) mod rfc3339 {
    use serde::Serializer;
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;

    /// Serialize as UTC.
    pub fn serialize<S: Serializer>(
        value: &OffsetDateTime,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let utc = value.to_offset(time::UtcOffset::UTC);
        serializer.serialize_str(&utc.format(&Rfc3339).map_err(serde::ser::Error::custom)?)
    }
}

/// Query parameters accepted by the public search endpoint (documentation
/// mirror — the handler validates a raw parameter map so it can report every
/// failing rule, not just the first).
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct SearchQuery {
    /// Prefix of letters/digits, optionally followed by a dot and digits.
    #[param(example = "ali")]
    pub prefix: String,
    /// Requested page size, defaulting to 100 and clamped to 1,000.
    #[param(example = 100)]
    pub limit: Option<u32>,
    /// Opaque continuation cursor from an earlier response.
    pub cursor: Option<String>,
}

/// One assigned username in the shipping mobile response shape.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchUsername {
    /// People Chain account encoded as SS58.
    #[schema(example = "5Example")]
    pub account_id: String,
    /// Full username when present, otherwise `base.digits` with the digits
    /// rendered as an integer (`alice.1` for stored digits `01`).
    #[schema(example = "alice.12")]
    pub username: String,
    /// Assignment state, always `ASSIGNED` for this projection.
    #[schema(example = "ASSIGNED")]
    pub status: &'static str,
    /// Time the projection first observed the account.
    #[serde(with = "rfc3339")]
    #[schema(value_type = String, format = DateTime, example = "2026-07-11T10:00:00Z")]
    pub created_at: OffsetDateTime,
    /// Time the projection last refreshed the account.
    #[serde(with = "rfc3339")]
    #[schema(value_type = String, format = DateTime, example = "2026-07-11T10:01:00Z")]
    pub updated_at: OffsetDateTime,
}

/// Public search response envelope.
#[derive(Debug, Clone, Serialize, ToSchema)]
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
    /// Query validation failed (field errors, in field order).
    #[error("invalid query parameters")]
    InvalidQuery(Vec<FieldError>),
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
///
/// Takes the raw query-parameter map so a validation failure can report every
/// failing rule, not just the first.
///
/// Rows whose on-chain `identifier_key` predates chat-spec RFC-0004 (any type
/// byte other than `0x00`/X25519) are omitted — the app cannot message those
/// accounts, and filtering server-side keeps pages full.
pub async fn search(
    pool: &PgPool,
    params: &HashMap<String, String>,
) -> Result<SearchResponse, SearchError> {
    let validated = validate(params)?;
    let pattern = format!("{}%", escape_like(&validated.prefix.to_lowercase()));
    let fetch_limit = i64::from(validated.limit) + 1;
    let cursor = validated.cursor;

    // The response `username` renders lite digits through `::text` (numeric),
    // so stored digits `01` render as `alice.1`; the digits-length bound hides
    // lite rows whose digit suffix is longer than two characters.
    //
    // The `get_byte(identifier_key, 0) = 0` bound hides registrations whose
    // on-chain encryption key predates chat-spec RFC-0004: the app can only
    // message peers whose 65-byte container carries the X25519 type byte
    // (`0x00`); pre-migration P-256 keys (SEC1 `0x04`) are unusable to it.
    // Filtering here keeps client pages full instead of making the app drop
    // rows post-pagination. The projection itself stays an unfiltered mirror
    // of the chain.
    //
    // TODO: remove the `get_byte(identifier_key, 0) = 0` predicate (and the
    // pre-RFC-0004-row expectation in `tests/pagination_live.rs`) once no
    // assigned usernames carry a pre-RFC-0004 key any more.
    let rows = sqlx::query(
        "SELECT account_id, account_id_ss58,
                COALESCE(full_username, lite_base || '.' || lite_digits::text)
                    AS wire_username,
                lower(display_username) COLLATE \"C\" AS display_key,
                lite_base, lite_digits::text AS lite_digits_text,
                created_at, updated_at
         FROM assigned_usernames
         WHERE lower(display_username) COLLATE \"C\" LIKE $1 ESCAPE '\\'
           AND (full_username IS NOT NULL OR display_username ~ '\\.[0-9]{1,2}$')
           AND get_byte(identifier_key, 0) = 0
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
    cursor: Option<Cursor>,
}

/// The prefix grammar `^[a-zA-Z0-9]+(\.\d*)?$`.
fn prefix_matches(prefix: &str) -> bool {
    let (head, tail) = match prefix.find('.') {
        Some(index) => (&prefix[..index], Some(&prefix[index + 1..])),
        None => (prefix, None),
    };
    !head.is_empty()
        && head.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && tail.is_none_or(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Validate the raw query map.
///
/// Order matters twice: the cursor is checked first, so a bad cursor wins over
/// a missing prefix, and field errors collect in field order (`prefix`, then
/// `limit`).
fn validate(params: &HashMap<String, String>) -> Result<ValidatedQuery, SearchError> {
    let cursor = params
        .get("cursor")
        .map(|raw| decode_cursor(raw))
        .transpose()?;

    let mut errors = Vec::new();

    let prefix = params.get("prefix");
    match prefix {
        None => errors.push(FieldError {
            message: http_common::error::expected("string", "nothing"),
            field: "prefix".to_string(),
        }),
        Some(prefix) => {
            // Every failing rule on the field is reported, in rule order
            // (min, max, pattern) — an empty prefix yields two errors.
            if prefix.is_empty() {
                errors.push(FieldError {
                    message: PREFIX_REQUIRED.to_string(),
                    field: "prefix".to_string(),
                });
            }
            if prefix.chars().count() > 64 {
                errors.push(FieldError {
                    message: PREFIX_TOO_LONG.to_string(),
                    field: "prefix".to_string(),
                });
            }
            if !prefix_matches(prefix) {
                errors.push(FieldError {
                    message: PREFIX_PATTERN.to_string(),
                    field: "prefix".to_string(),
                });
            }
        }
    }

    let limit = match params.get("limit") {
        None => DEFAULT_LIMIT,
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(0) => {
                errors.push(FieldError {
                    message: http_common::error::MUST_BE_POSITIVE.to_string(),
                    field: "limit".to_string(),
                });
                DEFAULT_LIMIT
            }
            Ok(limit) => limit,
            Err(_) => {
                errors.push(FieldError {
                    message: http_common::error::expected("a positive integer", raw),
                    field: "limit".to_string(),
                });
                DEFAULT_LIMIT
            }
        },
    };

    if !errors.is_empty() {
        return Err(SearchError::InvalidQuery(errors));
    }

    Ok(ValidatedQuery {
        prefix: prefix.cloned().unwrap_or_default(),
        // An over-large limit clamps to a full page — a 200, never a 400.
        limit: limit.min(MAX_LIMIT),
        cursor,
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
            username: row.try_get("wire_username")?,
            status: "ASSIGNED",
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
    use std::collections::HashMap;

    use time::macros::datetime;

    use super::{assemble_page, decode_cursor, encode_cursor, validate};
    use super::{Cursor, SearchError, SearchResponse, SearchRow, SearchUsername};

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn sample_row(index: u8) -> SearchRow {
        SearchRow {
            item: SearchUsername {
                account_id: format!("5Account{index}"),
                username: format!("alice.{index}"),
                status: "ASSIGNED",
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

    fn field_errors(result: Result<super::ValidatedQuery, SearchError>) -> Vec<(String, String)> {
        match result {
            Err(SearchError::InvalidQuery(errors)) => {
                errors.into_iter().map(|e| (e.field, e.message)).collect()
            }
            other => panic!("expected InvalidQuery, got {other:?}", other = other.err()),
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
    fn accepts_wire_queries_and_ignores_unknown_params() {
        let plain = validate(&params(&[("prefix", "ali")])).expect("valid query");
        assert_eq!(plain.limit, 100);

        // Unknown parameters are ignored (the Android client still sends
        // `status=ASSIGNED`).
        let with_unknown = validate(&params(&[("prefix", "ali"), ("status", "ASSIGNED")]))
            .expect("unknown params ignored");
        assert_eq!(with_unknown.prefix, "ali");
    }

    #[test]
    fn clamps_large_limits_instead_of_rejecting() {
        let clamped = validate(&params(&[("prefix", "ali"), ("limit", "5000")]))
            .expect("over-large limit clamps");
        assert_eq!(clamped.limit, 1000);
    }

    #[test]
    fn rejects_invalid_prefix_and_limit() {
        // Empty prefix reports BOTH the min and the pattern rule.
        assert_eq!(
            field_errors(validate(&params(&[("prefix", "")]))),
            vec![
                ("prefix".to_string(), "Prefix is required".to_string()),
                ("prefix".to_string(), super::PREFIX_PATTERN.to_string()),
            ]
        );
        assert_eq!(
            field_errors(validate(&params(&[]))),
            vec![(
                "prefix".to_string(),
                "expected string, received nothing".to_string()
            )]
        );
        assert_eq!(
            field_errors(validate(&params(&[("prefix", "foo-bar")]))),
            vec![("prefix".to_string(), super::PREFIX_PATTERN.to_string())]
        );

        for (raw, detail) in [
            ("abc", "expected a positive integer, received abc"),
            ("1.5", "expected a positive integer, received 1.5"),
            ("-1", "expected a positive integer, received -1"),
            ("0", "must be greater than 0"),
        ] {
            assert_eq!(
                field_errors(validate(&params(&[("prefix", "ali"), ("limit", raw)]))),
                vec![("limit".to_string(), detail.to_string())]
            );
        }

        // Multi-failure collects prefix errors before limit errors.
        let long_prefix = "a".repeat(65);
        assert_eq!(
            field_errors(validate(&params(&[
                ("prefix", long_prefix.as_str()),
                ("limit", "0"),
            ]))),
            vec![
                (
                    "prefix".to_string(),
                    "Prefix must be at most 64 characters".to_string()
                ),
                ("limit".to_string(), "must be greater than 0".to_string()),
            ]
        );
    }

    #[test]
    fn invalid_cursor_wins_over_other_validation_failures() {
        assert!(matches!(
            validate(&params(&[("cursor", "%%nope%%")])),
            Err(SearchError::InvalidCursor)
        ));
    }

    #[test]
    fn non_empty_response_matches_the_documented_shape() {
        let response = SearchResponse {
            usernames: vec![SearchUsername {
                account_id: "5Example".to_string(),
                username: "alice.12".to_string(),
                status: "ASSIGNED",
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
                    "createdAt": "2026-07-11T10:00:00Z",
                    "updatedAt": "2026-07-11T10:01:00Z"
                }],
                "nextCursor": "eyJvcGFxdWUiOiJjdXJzb3IifQ"
            })
        );
    }

    #[test]
    fn empty_response_matches_the_documented_shape() {
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
