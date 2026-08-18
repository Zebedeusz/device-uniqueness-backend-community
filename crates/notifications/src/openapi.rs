//! The OpenAPI document for the notifications service.
//!
//! The notify route is described by the `#[utoipa::path]` attribute on its
//! handler; the request/response bodies here are documentation-only mirrors of
//! the frozen wire shapes (the handler works on raw JSON to preserve the legacy
//! validation quirks byte-for-byte). The committed
//! `docs/api-reference/{openapi.json,index.html}` are generated from [`ApiDoc`]
//! by the `apidoc-gen` binary, so they cannot drift from the code.

use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

/// The frozen `POST /api/v1/notify` request body.
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the wire shape
pub struct NotifyRequestDoc {
    /// Recipient device token (APNs hex or FCM token); platform is auto-detected.
    #[schema(
        rename = "deviceToken",
        example = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
    )]
    pub device_token: String,
    /// Opaque push id echoed to the client app (32 or 64 hex chars).
    #[schema(rename = "pushId", example = "5d41402abc4b2a76b9719d911017c592")]
    pub push_id: String,
    /// Hex-encoded, already-encrypted message body (optional `0x` prefix).
    #[schema(example = "0x1234567890abcdef")]
    pub message: String,
    /// Explicit platform override; auto-detected from the token when omitted.
    #[schema(example = "ios", nullable)]
    pub platform: Option<String>,
    /// APNs topic override (the app bundle id); VoIP derives `<topic>.voip`.
    #[schema(rename = "bundlerId", example = "io.parity.brevity", nullable)]
    pub bundler_id: Option<String>,
    /// Enable the iOS VoIP push type for a call.
    #[schema(example = false, nullable)]
    pub voip: Option<bool>,
}

/// The frozen `200` push-result body (echoed from the provider).
#[derive(Serialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the wire shape
pub struct NotifyResponseDoc {
    /// Whether the provider accepted the push.
    #[schema(example = true)]
    pub success: bool,
    /// Platform the notification was routed to.
    #[schema(example = "ios")]
    pub platform: String,
    /// Number of notifications sent, when the provider reports it (APNs/FCM).
    #[schema(example = 1, nullable)]
    pub sent: Option<u32>,
    /// Number of failed notifications, when the provider reports it (APNs).
    #[schema(example = 0, nullable)]
    pub failed: Option<u32>,
    /// Provider message id, when returned (FCM).
    #[schema(rename = "messageId", nullable)]
    pub message_id: Option<String>,
    /// Per-device errors, on partial or full provider failure.
    #[schema(nullable)]
    pub errors: Option<Vec<NotifyErrorDoc>>,
}

/// One per-device push error inside the `200` response.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the wire shape
pub struct NotifyErrorDoc {
    /// Device token the error is about.
    pub device: String,
    /// APNs environment, when the provider distinguishes dev/prod.
    #[schema(nullable)]
    pub environment: Option<String>,
    /// Provider status (string or number), when present.
    #[schema(nullable)]
    pub status: Option<serde_json::Value>,
    /// Raw provider response, when present.
    #[schema(nullable)]
    pub response: Option<serde_json::Value>,
}

/// Adds the shared `bearer_jwt` security scheme referenced by the notify route.
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

/// The notifications OpenAPI document (merged into the workspace reference by
/// `apidoc-gen`).
#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "Notifications",
         description = "Send a push notification to an iOS (APNs) or Android (FCM) device. \
Stateless verify-only relay: it authenticates the JWT and forwards the frozen mobile payload, \
returning the provider's result — including the legacy `200 success:false` on provider failure.")
    ),
    paths(crate::notify::handle),
    components(schemas(NotifyRequestDoc, NotifyResponseDoc, NotifyErrorDoc)),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;
