//! The route table: who owns which path.
//!
//! This is the ownership map that lives in [`gateway/Caddyfile`]'s `(routes)`
//! snippet, expressed in Rust and checked by the compiler. It is what
//! `--role all-in-one` serves, and it is why a consumer running this binary
//! does not have to reconstruct the routing themselves.
//!
//! # How the merge reproduces the edge's table
//!
//! Most of it falls out for free, because the six services register **disjoint**
//! absolute paths. The one genuinely subtle row is `/api/v1/usernames`, which
//! the edge splits by METHOD between two services:
//!
//! | request | edge → owner | merged router |
//! |---|---|---|
//! | `GET /api/v1/usernames/search` | indexer | the indexer's route |
//! | `POST /api/v1/usernames/search` | not a read → attestation → no such route → JSON 404 | the indexer's route exists but is GET-only, and carries its own `fallback(not_found)` → the same JSON 404 |
//! | `GET /api/v1/usernames/available` | indexer → no such route → JSON 404 | attestation's route exists but is POST-only, and carries its own `fallback(not_found)` → the same JSON 404 |
//! | `POST /api/v1/usernames/available` | attestation | attestation's route |
//! | `GET /api/v1/usernames/payment-status` | carved out of the reads → attestation | attestation's route |
//! | `GET`/`POST`/`OPTIONS` `/api/v1/usernames` | indexer (GET) → JSON 404; attestation otherwise | attestation's collection root, POST/OPTIONS with a JSON-404 method fallback |
//! | `GET /api/v1/usernames/anything-else` | indexer → JSON 404 | no route → the global fallback → JSON 404 |
//!
//! Every row agrees, because attestation owns `/`, `/available` and
//! `/payment-status` while the indexer owns `/search`, and **both** answer an
//! unmatched method with the same JSON body. The method split at the edge
//! and the method routers here happen to agree — that is worth re-checking
//! whenever either side gains a route, which is what the route-table sweep in
//! this crate's tests is for.
//!
//! # The one row that does NOT fall out for free
//!
//! `notify-relay` speaks a different 404 dialect from every other service: its
//! `routes()` attaches no `fallback`, so an unmatched path under it answers
//! axum's **empty** 404 rather than the JSON body. The edge sends the whole
//! `/api/v1/notify*` prefix there, so that empty body is the live contract for
//! the prefix — and a global fallback would silently replace it. Hence
//! [`fallback`] below, which reproduces the edge's `handle /api/v1/notify*`
//! prefix match (no trailing slash required, exactly like Caddy's `*`).
//!
//! [`gateway/Caddyfile`]: ../../../../gateway/Caddyfile

pub mod table;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use http_common::error::not_found;

/// The prefix whose owner answers an empty 404 rather than the JSON body.
///
/// Read out of [`table::TABLE`] rather than written here a second time: the
/// generated edge configs and this runtime fallback must agree about where the
/// notify prefix starts, and one literal is how that stays true.
fn notify_prefix() -> &'static str {
    match table::TABLE
        .iter()
        .find(|row| row.owner == "notify-relay")
        .map(|row| row.matches)
    {
        Some(table::Match::Prefix(prefix)) => prefix,
        // The row is asserted to exist and to be a bare prefix by
        // `the_notify_row_is_a_bare_prefix` below, so this is unreachable in a
        // build that passes its tests.
        _ => "/api/v1/notify",
    }
}

/// The five services' routers, already stated.
pub struct Surfaces {
    /// Auth, attester, the usernames write surface, JWKS — and the global
    /// fallback owner: the edge's `handle { }` sends everything unclaimed here.
    pub attestation: Router,
    /// Username reads (`/search`) and proof-of-compute issuance.
    pub indexer: Router,
    /// `/api/v1/invitation-ticket/*`.
    pub invite_tickets: Router,
    /// `/api/v1/turn/*`.
    pub turn: Router,
    /// `/api/v1/notify*` — see the module docs for its distinct 404 dialect.
    pub notifications: Router,
}

/// Merge the five surfaces into the public route table.
///
/// The caller adds health, `/docs` and the middleware stack; this function owns
/// only who-answers-what.
pub fn merge(surfaces: Surfaces) -> Router {
    Router::new()
        .merge(surfaces.attestation)
        .merge(surfaces.indexer)
        .merge(surfaces.invite_tickets)
        .merge(surfaces.turn)
        .merge(surfaces.notifications)
        .fallback(fallback)
}

/// The service-wide fallback, reproducing the edge's per-prefix ownership for
/// an unmatched path: the notify prefix answers its owner's empty 404, and
/// everything else answers device attestation's JSON 404 body — that service being the
/// edge's catch-all.
async fn fallback(request: Request) -> Response {
    if request.uri().path().starts_with(notify_prefix()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    not_found().await.into_response()
}

#[cfg(test)]
mod tests {
    use super::{notify_prefix, table};

    /// The runtime fallback reads the prefix out of the table, and only a bare
    /// `Prefix` row has the "no trailing slash required" semantics Caddy's `*`
    /// gives it. If this row ever became `PrefixSlash`, `/api/v1/notifyfoo`
    /// would silently change dialect.
    #[test]
    fn the_notify_row_is_a_bare_prefix() {
        let row = table::TABLE
            .iter()
            .find(|row| row.owner == "notify-relay")
            .expect("the notify row exists");
        assert!(matches!(
            row.matches,
            table::Match::Prefix("/api/v1/notify")
        ));
        assert_eq!(notify_prefix(), "/api/v1/notify");
    }

    /// The edge matches `/api/v1/notify*` as a PREFIX, so the Rust side must
    /// too — a path-segment match would hand `/api/v1/notifyfoo` to the wrong
    /// dialect.
    #[test]
    fn the_notify_prefix_matches_the_way_the_edge_does() {
        for path in [
            "/api/v1/notify",
            "/api/v1/notify/",
            "/api/v1/notify/bogus",
            "/api/v1/notifyfoo",
        ] {
            assert!(path.starts_with(notify_prefix()), "{path}");
        }
        for path in ["/api/v1/notif", "/api/v1/usernames", "/api/v1/turn/issue"] {
            assert!(!path.starts_with(notify_prefix()), "{path}");
        }
    }
}
