//! `GET /api/v1/usernames/search` — assigned-username prefix search (public).
//!
//! Kept public (shipping iOS calls it unauthenticated); rate-limited. Real
//! prefix results need the finalized-state index, which lands in Slice 7
//! (`username-read`) — until then this returns the correct empty shape so
//! clients parse a valid response. Each result will be
//! `{accountId, username, status, onchainData?, createdAt, updatedAt}`.

use axum::extract::Query;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::http::error::{AppError, AppResult};

#[derive(Deserialize)]
pub struct SearchQuery {
    prefix: Option<String>,
}

#[derive(Serialize)]
pub struct SearchResponse {
    usernames: Vec<serde_json::Value>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

/// Search assigned usernames by prefix. Empty until the indexer lands (Slice 7).
pub async fn search(Query(query): Query<SearchQuery>) -> AppResult<Json<SearchResponse>> {
    let prefix = query.prefix.unwrap_or_default();
    if prefix.trim().is_empty() {
        return Err(AppError::bad_request("prefix is required"));
    }
    Ok(Json(SearchResponse {
        usernames: Vec::new(),
        next_cursor: None,
    }))
}
