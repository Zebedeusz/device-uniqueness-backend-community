//! The OpenAPI document for the turn service.
//!
//! The issue route is described by the `#[utoipa::path]` attribute on its
//! handler; the request/response bodies here are documentation-only mirrors
//! of the wire shapes (the handler works on raw JSON so it can report every
//! failing field rather than the first). The committed
//! `docs/api-reference/{openapi.json,index.html}` are generated from
//! [`ApiDoc`] by the `apidoc-gen` binary, so they cannot drift from the code.

use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

/// `{ regionHint? }` — the issue command (the hint is reserved and ignored).
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the wire shape
pub struct IssueRequest {
    /// Optional region hint (reserved for future use; accepted and ignored).
    #[serde(rename = "regionHint")]
    #[schema(rename = "regionHint", example = "eu-west", nullable)]
    pub region_hint: Option<String>,
}

/// The 201 body: coturn REST-API ephemeral credentials plus the ICE
/// server list.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the wire shape
pub struct IssueResponse {
    /// Configured ICE server URLs (`stun:` / `turn:` forms), echoed verbatim.
    #[schema(example = json!(["stun:stun.example.com:3478", "turn:turn.example.com:3478?transport=udp"]))]
    pub servers: Vec<String>,
    /// `{unixExpiry}:{hexId}`. JWT issuance uses mint time + configured TTL
    /// and 8 random id bytes; proof issuance uses mint time + configured TTL
    /// and a deterministic opaque 16-byte id derived from product and alias.
    #[schema(example = "1784757652:0a79e3412921701a")]
    pub username: String,
    /// Base64 HMAC over `username` under the relay-shared secret (algorithm
    /// per deployment config, default HMAC-SHA1).
    #[schema(example = "qmg5g7d1bXzY0qZkRUqtIPEIKjA=")]
    pub password: String,
    /// Credential time-to-live in whole seconds (the configured `ttl_secs`).
    #[schema(example = 1800)]
    pub ttl: u64,
}

/// Adds the shared `bearer_jwt` security scheme referenced by the issue route.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            "bearer_jwt",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some("Access JWT issued by POST /api/v1/auth/token."))
                    .build(),
            ),
        );
    }
}

/// The turn OpenAPI document (merged into the workspace reference by
/// `apidoc-gen`).
#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "TURN",
         description = "Short-lived TURN credentials for WebRTC ICE negotiation: the coturn \
REST-API construction (username = expiry:id, password = HMAC over the username) minted \
against a secret shared with the TURN relay. Stateless — nothing is stored. Issuance is \
authorized either by an access JWT (`/issue`) or, when enabled, by a personhood ring-VRF \
proof over a client-timestamped message (`/issue-with-proof`).")
    ),
    paths(
        crate::http::issue_credentials,
        crate::http::proof_routes::issue_with_proof,
    ),
    components(schemas(
        IssueRequest,
        IssueResponse,
        crate::http::proof_routes::IssueWithProofBody,
    )),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;
