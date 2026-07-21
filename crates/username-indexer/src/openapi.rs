//! OpenAPI fragment for the username-indexer public surface.
//!
//! Only the public prefix-search endpoint is documented here; it is merged into
//! the unified API reference alongside the identity-service document. Health
//! probes are intentionally omitted from the published reference.

use utoipa::OpenApi;

/// The username-indexer OpenAPI document (search endpoint only).
#[derive(OpenApi)]
#[openapi(
    paths(crate::http::search_usernames),
    components(schemas(crate::search::SearchResponse, crate::search::SearchUsername,))
)]
pub struct ApiDoc;
