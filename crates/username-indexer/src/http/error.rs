//! Stable HTTP error mapping for public handlers.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::search::SearchError;

/// Handler error mapped to a public JSON response.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Query parameters or cursor were invalid.
    #[error("{0}")]
    BadRequest(String),
    /// Client exceeded the public search rate limit.
    #[error("rate limited")]
    RateLimited,
    /// An internal dependency failed.
    #[error("internal error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "WRONG_DATA", "message": message })),
            )
                .into_response(),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "error": "RATE_LIMITED", "message": "rate limited" })),
            )
                .into_response(),
            Self::Internal(error) => {
                tracing::error!(error = ?error, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "INTERNAL", "message": "internal error" })),
                )
                    .into_response()
            }
        }
    }
}

impl From<SearchError> for AppError {
    fn from(error: SearchError) -> Self {
        match error {
            SearchError::InvalidQuery(message) => Self::BadRequest(message),
            SearchError::InvalidCursor => Self::BadRequest("invalid cursor".to_string()),
            SearchError::Database(error) => Self::Internal(Box::new(error)),
        }
    }
}

/// Result returned by HTTP handlers.
pub type AppResult<T> = Result<T, AppError>;
