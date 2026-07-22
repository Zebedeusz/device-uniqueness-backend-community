//! The OpenAPI document for the dim-tickets service.
//!
//! The two routes are described by the `#[utoipa::path]` attributes on their
//! handlers; the request/response bodies here are documentation-only mirrors
//! of the frozen wire shapes (the handlers themselves work on raw JSON to
//! preserve the legacy validation quirks byte-for-byte). The committed
//! `docs/api-reference/{openapi.json,index.html}` are generated from
//! [`ApiDoc`] by the `apidoc-gen` binary, so they cannot drift from the code.

use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

/// `{ who, dim }` — the ticket request command.
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the frozen wire shape
pub struct CreateTicketRequest {
    /// SS58 address to request a ticket for. Only non-emptiness is validated
    /// here; SS58 validity is checked later (an invalid address is a 500 —
    /// legacy quirk).
    #[schema(example = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty")]
    pub who: String,
    /// DIM to request a ticket for: `Game` or `ProofOfInk`.
    #[schema(example = "Game")]
    pub dim: String,
}

/// The frozen 200 body of `POST /api/v1/dim-ticket`: the accepted `PENDING`
/// ticket (no `onchainData` key; the address echoes as both `ticket` and
/// `who`).
#[derive(Serialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the frozen wire shape
pub struct CreateTicketResponse {
    /// SS58 address of the ticket (echoes the requested `who`).
    #[schema(example = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty")]
    pub ticket: String,
    /// SS58 address who requested the ticket (same value as `ticket`).
    #[schema(example = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty")]
    pub who: String,
    /// SS58 address of the inviter whose on-chain quota funds the ticket.
    #[schema(example = "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM")]
    pub inviter: String,
    /// Network the ticket will be registered on: `westend2`, `paseo`, or
    /// `polkadot`.
    #[schema(example = "paseo")]
    pub network: String,
    /// DIM the ticket funds: `Game` or `ProofOfInk`.
    #[schema(example = "Game")]
    pub dim: String,
    /// Always `PENDING` on creation.
    #[schema(example = "PENDING")]
    pub status: String,
    /// Deprecated mirror of `status == "REGISTERED"` — always `false` here.
    #[schema(example = false)]
    pub registered: bool,
    /// Ticket creation time (JS-style ISO, millisecond precision).
    #[serde(rename = "createdAt")]
    #[schema(rename = "createdAt", example = "2026-07-01T10:20:30.400Z")]
    pub created_at: String,
    /// Response time (JS-style ISO, millisecond precision).
    #[serde(rename = "updatedAt")]
    #[schema(rename = "updatedAt", example = "2026-07-01T10:20:30.400Z")]
    pub updated_at: String,
}

/// On-chain registration data, present once a ticket is `REGISTERED`.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the frozen wire shape
pub struct OnchainData {
    /// `blockNumber-eventIndex` composite index.
    #[serde(rename = "blockIndex")]
    #[schema(rename = "blockIndex", example = "12345-7")]
    pub block_index: Option<String>,
    /// Block number of the registration event.
    #[serde(rename = "blockNumber")]
    #[schema(rename = "blockNumber", example = 12345)]
    pub block_number: Option<u64>,
    /// Hash of the block containing the registration event.
    #[serde(rename = "blockHash")]
    #[schema(
        rename = "blockHash",
        example = "0xabababababababababababababababababababababababababababababababab"
    )]
    pub block_hash: Option<String>,
    /// Index of the registration event inside the block.
    #[serde(rename = "eventIndex")]
    #[schema(rename = "eventIndex", example = 7)]
    pub event_index: Option<u64>,
}

/// The frozen 200 body of `GET /api/v1/dim-ticket/{who}`.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)] // documentation-only mirror of the frozen wire shape
pub struct TicketStatusResponse {
    /// SS58 address of the ticket.
    #[schema(example = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty")]
    pub ticket: String,
    /// SS58 address of the inviter.
    #[schema(example = "5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM")]
    pub inviter: String,
    /// Network the ticket is registered on: `westend2`, `paseo`, or
    /// `polkadot`.
    #[schema(example = "paseo")]
    pub network: String,
    /// DIM the ticket funds: `Game` or `ProofOfInk`.
    #[schema(example = "Game")]
    pub dim: String,
    /// `PENDING`, `SUBMITTED`, `REGISTERED`, or `FAILED` (DB `SUBMITTING`
    /// reports as `SUBMITTED`).
    #[schema(example = "REGISTERED")]
    pub status: String,
    /// Deprecated mirror of `status == "REGISTERED"` — use `status` instead.
    #[schema(example = true)]
    pub registered: bool,
    /// On-chain registration data; `null` until `REGISTERED`.
    #[serde(rename = "onchainData")]
    #[schema(rename = "onchainData")]
    pub onchain_data: Option<OnchainData>,
    /// Ticket creation time (JS-style ISO, millisecond precision).
    #[serde(rename = "createdAt")]
    #[schema(rename = "createdAt", example = "2026-07-01T10:20:30.400Z")]
    pub created_at: String,
    /// Last state-change time (JS-style ISO; a `PENDING` ticket reports the
    /// response time — legacy quirk).
    #[serde(rename = "updatedAt")]
    #[schema(rename = "updatedAt", example = "2026-07-02T11:00:00.000Z")]
    pub updated_at: String,
}

/// Adds the shared `bearer_jwt` security scheme referenced by both routes.
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

/// The dim-tickets OpenAPI document (merged into the workspace reference by
/// `apidoc-gen`).
#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "DIM Tickets",
         description = "Asynchronous DIM (Game / ProofOfInk) invitation-credential requests: \
a ticket is accepted as PENDING and registered on People Chain by the single-instance \
chain writer; clients poll the status route.")
    ),
    paths(crate::http::create_ticket, crate::http::get_ticket),
    components(schemas(CreateTicketRequest, CreateTicketResponse, TicketStatusResponse, OnchainData)),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;
