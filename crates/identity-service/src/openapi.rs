//! The OpenAPI document for the identity service.
//!
//! This is the single source of truth for the public HTTP contract: every route
//! is described by the `#[utoipa::path]` attribute on its handler, and every
//! request/response body by `#[derive(ToSchema)]` on its type. The committed
//! `docs/api-reference/openapi.json` and the rendered `index.html` are generated
//! from [`ApiDoc`] by the `apidoc-gen` binary, so they cannot drift from the code.

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// Adds the shared `bearer_jwt` security scheme referenced by JWT-gated routes.
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

/// The full identity-service OpenAPI document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Identity Backend V2",
        version = "2",
        description = "**Prove the device and register the username on People Chain.** \
API-compatible with the shipping iOS/Android clients.\n\n\
### Validation flow\n\n\
The iOS smoke drives this sequence end-to-end; each step gates the next:\n\n\
1. `POST /api/v1/auth/challenges` — mint a single-use challenge\n\
2. `POST /api/v1/auth/app-attest/attestations` — iOS App Attest (`202`, no-op in M0)\n\
3. `POST /api/v1/auth/token` — sr25519 proof exchanged for a JWT\n\
4. `POST /api/v1/usernames/available` — JWT-gated People Chain read\n\
5. `POST /api/v1/usernames` — `202` outbox reservation\n\n\
> **Attestation is disabled for M0.** Apple/Google verification is intentionally a \
no-op; the sr25519 account proof and People Chain reads still run end-to-end.\n\n\
All non-2xx responses share the `ErrorResponse` envelope: `{ error, message }`."
    ),
    servers(
        (url = "https://identity.dotspark.app", description = "Test deployment")
    ),
    tags(
        (name = "Liveness & Readiness", description = "Process, liveness, and readiness probes."),
        (name = "Discovery", description = "Public keyset and attester authority."),
        (name = "Authentication", description = "The attestation → JWT handshake."),
        (name = "Usernames", description = "Availability reads and registration intake.")
    ),
    paths(
        crate::http::health::healthcheck,
        crate::http::health::livez,
        crate::http::health::readyz,
        crate::http::jwks,
        crate::usernames::attester,
        crate::auth::challenge::issue,
        crate::auth::app_attest::register,
        crate::auth::token::issue,
        crate::auth::refresh::rotate,
        crate::usernames::available::check,
        crate::usernames::register::register,
        crate::queue::status,
    ),
    components(schemas(
        crate::http::error::ErrorResponse,
        crate::http::health::Status,
        crate::auth::challenge::ChallengeResponse,
        crate::auth::app_attest::AppAttestRequest,
        crate::auth::token::TokenResponse,
        crate::auth::refresh::RefreshRequest,
        crate::usernames::AttesterResponse,
        crate::usernames::available::AvailableRequest,
        crate::usernames::available::AvailableV1Response,
        crate::usernames::available::NameAvailability,
        crate::usernames::register::RegisterRequest,
        crate::usernames::register::Dotns,
        crate::usernames::register::RegisterResponse,
        crate::queue::QueueStatusResponse,
    )),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;
