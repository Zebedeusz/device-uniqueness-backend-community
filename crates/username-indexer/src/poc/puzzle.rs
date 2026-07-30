//! Puzzle issuance: the HMAC-signed challenge and its secret derivation.

use hmac::{Hmac, Mac as _};
use serde::Serialize;
use sha2::Sha256;
use uuid::Uuid;

/// HKDF `info` string legacy used to derive the puzzle HMAC key. Kept
/// byte-identical so a puzzle issued by either stack verifies in the other
/// when both are configured with the same input keying material.
const HKDF_INFO: &[u8] = b"identity-backend/proof-of-compute/hmac-v1";

type HmacSha256 = Hmac<Sha256>;

/// Derive the 32-byte puzzle HMAC key from input keying material.
pub fn derive_secret(ikm: &str) -> [u8; 32] {
    let hkdf = hkdf::Hkdf::<Sha256>::new(None, ikm.as_bytes());
    let mut secret = [0u8; 32];
    hkdf.expand(HKDF_INFO, &mut secret)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    secret
}

/// A freshly issued puzzle, serialized as the frozen `201` body.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Puzzle {
    /// Opaque puzzle identifier (UUID); the replay-protection key.
    #[schema(value_type = String, example = "1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed")]
    pub session_id: Uuid,
    /// Issue time in unix milliseconds; bounds the puzzle's validity.
    #[schema(example = 1700000000000i64)]
    pub timestamp: i64,
    /// Required number of leading zero bits in the work digest.
    #[schema(minimum = 1, maximum = 32, example = 16)]
    pub difficulty: u8,
    /// Lowercase-hex HMAC-SHA256 over the puzzle fields; proves this server
    /// issued the puzzle, so nothing is stored at issue time.
    #[schema(example = "c8828951fd6c123fdbf6501f111d27dd3f260839344a7370e0dd8f20e2c40482")]
    pub checksum: String,
}

impl Puzzle {
    /// Build and sign a puzzle for the given session, time, and difficulty.
    pub fn new(secret: &[u8], session_id: Uuid, timestamp_ms: i64, difficulty: u8) -> Self {
        Self {
            checksum: checksum_hex(secret, session_id, timestamp_ms, difficulty),
            session_id,
            timestamp: timestamp_ms,
            difficulty,
        }
    }
}

/// The checksum preimage: 16 UUID bytes `||` timestamp as u64 big-endian `||`
/// difficulty as one byte (legacy `proof-of-compute.schema.ts`).
fn checksum_preimage(session_id: Uuid, timestamp_ms: i64, difficulty: u8) -> [u8; 25] {
    let mut preimage = [0u8; 25];
    preimage[..16].copy_from_slice(session_id.as_bytes());
    preimage[16..24].copy_from_slice(&(timestamp_ms as u64).to_be_bytes());
    preimage[24] = difficulty;
    preimage
}

/// Lowercase-hex HMAC-SHA256 over [`checksum_preimage`].
pub fn checksum_hex(secret: &[u8], session_id: Uuid, timestamp_ms: i64, difficulty: u8) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&checksum_preimage(session_id, timestamp_ms, difficulty));
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{checksum_hex, derive_secret, Puzzle};

    /// The known-answer vector embedded in legacy
    /// `issue-proof-of-compute.executor.ts` — the guard against a silent
    /// preimage/endianness drift.
    ///
    /// Note the vector keys the HMAC with the secret bytes **directly**: legacy
    /// applies HKDF once at config time (see [`derive_secret`]) and passes the
    /// derived key here, so its test vector uses a literal key.
    #[test]
    fn matches_the_legacy_checksum_vector() {
        let session_id = Uuid::parse_str("1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed").expect("uuid");
        assert_eq!(
            checksum_hex(b"vector-secret", session_id, 1_700_000_000_000, 4),
            "c8828951fd6c123fdbf6501f111d27dd3f260839344a7370e0dd8f20e2c40482"
        );
    }

    /// RFC 5869 HKDF-SHA256 with a zero salt and legacy's `info` string — the
    /// derivation legacy runs over `JWT_AUTH_SECRET` before HMAC-ing puzzles.
    /// Pinned so the two stacks can verify each other's puzzles when configured
    /// with the same input keying material.
    #[test]
    fn derives_the_hkdf_key_from_input_keying_material() {
        assert_eq!(
            hex::encode(derive_secret("vector-secret")),
            "7fff0decbb00336c3174e955d28fa0eba72c935451d9100e6cea5f314d237eba"
        );
    }

    #[test]
    fn puzzle_carries_the_signed_fields() {
        let secret = derive_secret("vector-secret");
        let session_id = Uuid::parse_str("1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed").expect("uuid");
        let puzzle = Puzzle::new(&secret, session_id, 1_700_000_000_000, 4);
        assert_eq!(puzzle.session_id, session_id);
        assert_eq!(puzzle.timestamp, 1_700_000_000_000);
        assert_eq!(puzzle.difficulty, 4);
        assert_eq!(
            puzzle.checksum,
            checksum_hex(&secret, session_id, 1_700_000_000_000, 4)
        );
    }
}
