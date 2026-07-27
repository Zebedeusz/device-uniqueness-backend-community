//! OpenAPI fragment for the username-indexer public surface.
//!
//! The public prefix-search endpoint is documented here and merged into the
//! unified API reference alongside the identity-service document. Health
//! probes are intentionally omitted from the published reference.

use utoipa::OpenApi;

/// The username-indexer OpenAPI document (public read endpoints).
#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "Usernames", description = "Public username search over the finalized chain projection. Unauthenticated; IP rate-limited.")
    ),
    paths(crate::http::search_usernames),
    components(schemas(
        crate::search::SearchResponse,
        crate::search::SearchUsername,
    ))
)]
pub struct ApiDoc;
