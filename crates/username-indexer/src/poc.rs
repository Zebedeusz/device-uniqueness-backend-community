//! Proof-of-compute: stateless puzzle issuance and one-shot verification.
//!
//! The gate protects the public, unauthenticated `GET /api/v1/usernames/search`
//! read for callers that hold no bearer token (the Desktop/host path). It is
//! **either-or**: a valid identity-service JWT satisfies the route just as a
//! solved puzzle does, which is why the shipping mobile apps — both of which
//! authenticate search — need no client change.
//!
//! The wire contract is ported byte-for-byte from the legacy TypeScript
//! backend (`paritytech/identity-backend`,
//! `src/proof-of-compute/*`, `src/middleware/proof-of-compute.middleware.ts`):
//! issuance is a signed (HMAC) puzzle so no state is written, and the work
//! function is leading-zero-bit search over SHA-256.
//!
//! Two deliberate divergences from legacy:
//! - replay protection is a Postgres consume ([`store`]) instead of legacy's
//!   per-process in-memory map, so it holds across replicas;
//! - a valid bearer token bypasses the puzzle (legacy intended this via route
//!   composition but never wired the middleware onto any route).

pub mod puzzle;
pub mod solution;
pub mod store;

use sqlx::PgPool;
use uuid::Uuid;

pub use puzzle::{derive_secret, Puzzle};
pub use solution::{leading_zero_bits, Solution};

/// Why a proof-of-compute-gated request was refused.
///
/// Each variant carries the exact legacy `detail` string and status: a
/// malformed header is a `400`, everything else is a `402`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    /// The `Proof-Of-Compute` header could not be decoded into a solution.
    #[error("malformed proof-of-compute header")]
    Malformed,
    /// No `Proof-Of-Compute` header and no valid bearer token.
    #[error("proof of compute required")]
    Missing,
    /// The checksum does not match: this server did not issue the puzzle.
    #[error("proof checksum mismatch")]
    Checksum,
    /// The puzzle is older than the configured TTL (plus clock skew).
    #[error("proof of compute puzzle expired")]
    Expired,
    /// The puzzle was already spent (cross-replica, via Postgres).
    #[error("proof of compute puzzle already used")]
    Replayed,
    /// The solution does not reach the declared difficulty.
    #[error("insufficient proof of compute work")]
    InsufficientWork,
}

impl Rejection {
    /// The legacy `detail` string for this rejection, byte-for-byte.
    pub fn detail(self) -> &'static str {
        match self {
            Rejection::Malformed => "The Proof-Of-Compute header is malformed.",
            Rejection::Missing => {
                "Proof of compute required. Request a puzzle from POST /api/v1/poc/issue \
                 and present the solved proof in the Proof-Of-Compute header."
            }
            Rejection::Checksum => {
                "The proof checksum does not match; the puzzle was not issued by this server."
            }
            Rejection::Expired => "The proof of compute puzzle has expired; request a new one.",
            Rejection::Replayed => "The proof of compute puzzle has already been used.",
            Rejection::InsufficientWork => {
                "The proof of compute solution does not meet the required difficulty."
            }
        }
    }
}

/// The configured gate: puzzle parameters, the HMAC secret, and the verify-only
/// JWT material used for the bearer bypass.
///
/// Present in `AppState` only when `POC_ENABLED=true`; when it is absent the
/// service behaves exactly as it did before this slice.
#[derive(Clone)]
pub struct Poc {
    secret: [u8; 32],
    difficulty: u8,
    ttl_secs: u32,
    clock_skew_secs: u32,
    jwt: jwt_verify::Verifier,
}

impl std::fmt::Debug for Poc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Poc")
            .field("secret", &"<redacted>")
            .field("difficulty", &self.difficulty)
            .field("ttl_secs", &self.ttl_secs)
            .field("clock_skew_secs", &self.clock_skew_secs)
            .finish()
    }
}

impl Poc {
    /// Build the gate from the derived HMAC secret, puzzle parameters, and the
    /// verify-only JWT verifier that admits authenticated callers.
    pub fn new(
        secret: [u8; 32],
        difficulty: u8,
        ttl_secs: u32,
        clock_skew_secs: u32,
        jwt: jwt_verify::Verifier,
    ) -> Self {
        Self {
            secret,
            difficulty,
            ttl_secs,
            clock_skew_secs,
            jwt,
        }
    }

    /// The verifier used for the bearer bypass.
    pub fn jwt(&self) -> &jwt_verify::Verifier {
        &self.jwt
    }

    /// Issue a fresh puzzle for `now_ms`, stamped with the configured difficulty.
    pub fn issue_at(&self, session_id: Uuid, now_ms: i64) -> Puzzle {
        Puzzle::new(&self.secret, session_id, now_ms, self.difficulty)
    }

    /// Issue a fresh puzzle for the current time with a random session id.
    pub fn issue(&self) -> Puzzle {
        self.issue_at(Uuid::new_v4(), now_millis())
    }

    /// Verify everything that needs no database: checksum, then expiry, then
    /// work — the legacy order, so the reported reason matches legacy's.
    pub fn verify_offline(&self, solution: &Solution, now_ms: i64) -> Result<(), Rejection> {
        if !solution.checksum_matches(&self.secret) {
            return Err(Rejection::Checksum);
        }
        let max_age = i64::from(self.ttl_secs) * 1_000 + i64::from(self.clock_skew_secs) * 1_000;
        // No lower bound, as legacy: a future timestamp is accepted because the
        // checksum already proves this server issued it.
        if now_ms - solution.timestamp_ms() > max_age {
            return Err(Rejection::Expired);
        }
        if solution.work_bits() < u32::from(solution.difficulty()) {
            return Err(Rejection::InsufficientWork);
        }
        Ok(())
    }

    /// Full verification: offline checks, then the cross-replica one-shot
    /// consume. A session id can be spent exactly once.
    pub async fn verify(
        &self,
        pool: &PgPool,
        solution: &Solution,
        now_ms: i64,
    ) -> Result<Result<(), Rejection>, sqlx::Error> {
        if let Err(rejection) = self.verify_offline(solution, now_ms) {
            return Ok(Err(rejection));
        }
        let retain_until_ms = solution.timestamp_ms()
            + i64::from(self.ttl_secs) * 1_000
            + i64::from(self.clock_skew_secs) * 1_000;
        if store::consume(pool, solution.session_id(), retain_until_ms).await? {
            Ok(Ok(()))
        } else {
            Ok(Err(Rejection::Replayed))
        }
    }
}

/// Current unix time in milliseconds (the puzzle timestamp unit).
pub fn now_millis() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}
