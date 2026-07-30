//! Solution parsing, the work function, and the header wire format.

use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::puzzle::checksum_hex;
use super::Rejection;

/// The header the solved puzzle travels in (legacy name, case-insensitive on
/// the wire).
pub const HEADER: &str = "proof-of-compute";

/// Legacy's upper bound on the puzzle timestamp (`8_640_000_000_000_000` ms).
const MAX_TIMESTAMP_MS: i64 = 8_640_000_000_000_000;

/// A parsed, not-yet-verified solution presented by a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Solution {
    session_id: Uuid,
    timestamp_ms: i64,
    difficulty: u8,
    counter: u64,
    checksum: String,
}

impl Solution {
    /// Decode the `Proof-Of-Compute` header value:
    /// `base64(sessionId:timestamp:difficulty:counter:checksum)`.
    ///
    /// Every field is range-checked exactly as legacy's schema did; anything
    /// off-shape is [`Rejection::Malformed`] (a `400`), never a `402`.
    pub fn parse_header(raw: &str) -> Result<Self, Rejection> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .map_err(|_| Rejection::Malformed)?;
        let text = String::from_utf8(decoded).map_err(|_| Rejection::Malformed)?;

        let mut parts = text.split(':');
        let (
            Some(session_id),
            Some(timestamp),
            Some(difficulty),
            Some(counter),
            Some(checksum),
            None,
        ) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        )
        else {
            return Err(Rejection::Malformed);
        };

        let session_id = Uuid::parse_str(session_id).map_err(|_| Rejection::Malformed)?;
        let timestamp_ms: i64 = timestamp.parse().map_err(|_| Rejection::Malformed)?;
        if !(0..=MAX_TIMESTAMP_MS).contains(&timestamp_ms) {
            return Err(Rejection::Malformed);
        }
        let difficulty: u8 = difficulty.parse().map_err(|_| Rejection::Malformed)?;
        if !(1..=32).contains(&difficulty) {
            return Err(Rejection::Malformed);
        }
        let counter: u64 = counter.parse().map_err(|_| Rejection::Malformed)?;
        if checksum.len() != 64 || !checksum.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Rejection::Malformed);
        }

        Ok(Self {
            session_id,
            timestamp_ms,
            difficulty,
            counter,
            checksum: checksum.to_ascii_lowercase(),
        })
    }

    /// Encode back to the header value (used by the solver example and tests).
    pub fn to_header(&self) -> String {
        let Self {
            session_id,
            timestamp_ms,
            difficulty,
            counter,
            checksum,
        } = self;
        base64::engine::general_purpose::STANDARD.encode(format!(
            "{session_id}:{timestamp_ms}:{difficulty}:{counter}:{checksum}"
        ))
    }

    /// Build a solution from puzzle fields and a mined counter.
    pub fn new(
        session_id: Uuid,
        timestamp_ms: i64,
        difficulty: u8,
        counter: u64,
        checksum: String,
    ) -> Self {
        Self {
            session_id,
            timestamp_ms,
            difficulty,
            counter,
            checksum,
        }
    }

    /// The replay-protection key.
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Issue time claimed by the solution (bound by the checksum).
    pub fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    /// Difficulty claimed by the solution (bound by the checksum).
    pub fn difficulty(&self) -> u8 {
        self.difficulty
    }

    /// Whether the checksum was produced by this server's key.
    pub fn checksum_matches(&self, secret: &[u8]) -> bool {
        let expected = checksum_hex(secret, self.session_id, self.timestamp_ms, self.difficulty);
        // Both sides are fixed-length lowercase hex; compare in constant time.
        expected.len() == self.checksum.len()
            && expected
                .bytes()
                .zip(self.checksum.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }

    /// Leading zero bits achieved by this solution's counter.
    pub fn work_bits(&self) -> u32 {
        leading_zero_bits(self.session_id, self.timestamp_ms, self.counter)
    }
}

/// The work preimage: 16 UUID bytes `||` timestamp as u64 big-endian `||`
/// counter as u64 big-endian (legacy `proof-of-compute.schema.ts`).
fn work_preimage(session_id: Uuid, timestamp_ms: i64, counter: u64) -> [u8; 32] {
    let mut preimage = [0u8; 32];
    preimage[..16].copy_from_slice(session_id.as_bytes());
    preimage[16..24].copy_from_slice(&(timestamp_ms as u64).to_be_bytes());
    preimage[24..].copy_from_slice(&counter.to_be_bytes());
    preimage
}

/// Leading zero bits of `sha256(work_preimage)`.
///
/// Counted over the leading big-endian `u32` of the digest, matching legacy's
/// `Math.clz32` — which is also why difficulty is capped at 32.
pub fn leading_zero_bits(session_id: Uuid, timestamp_ms: i64, counter: u64) -> u32 {
    let digest = Sha256::digest(work_preimage(session_id, timestamp_ms, counter));
    let leading = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    leading.leading_zeros()
}

/// Search counters from zero for one reaching `difficulty` leading zero bits.
///
/// Shared by the solver example and the tests so both mine exactly what the
/// verifier accepts.
pub fn mine(session_id: Uuid, timestamp_ms: i64, difficulty: u8) -> u64 {
    (0u64..)
        .find(|counter| {
            leading_zero_bits(session_id, timestamp_ms, *counter) >= u32::from(difficulty)
        })
        .expect("a counter exists for any difficulty <= 32")
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{leading_zero_bits, Solution};
    use crate::poc::puzzle::{checksum_hex, derive_secret};
    use crate::poc::Rejection;

    fn session() -> Uuid {
        Uuid::parse_str("1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed").expect("uuid")
    }

    /// Work vectors embedded in legacy `verify-proof-of-compute.executor.ts`.
    #[test]
    fn matches_the_legacy_work_vectors() {
        assert_eq!(leading_zero_bits(session(), 1_700_000_000_000, 0), 3);
        assert_eq!(leading_zero_bits(session(), 1_700_000_000_000, 12_345), 0);
    }

    #[test]
    fn header_round_trips() {
        let secret = derive_secret("vector-secret");
        let checksum = checksum_hex(&secret, session(), 1_700_000_000_000, 4);
        let solution = Solution::new(session(), 1_700_000_000_000, 4, 7, checksum);
        let parsed = Solution::parse_header(&solution.to_header()).expect("parses");
        assert_eq!(parsed, solution);
        assert!(parsed.checksum_matches(&secret));
    }

    #[test]
    fn rejects_off_shape_headers_as_malformed() {
        let secret = derive_secret("vector-secret");
        let checksum = checksum_hex(&secret, session(), 1_700_000_000_000, 4);
        let cases = [
            "not base64!!".to_string(),
            // too few fields
            b64("a:b:c:d"),
            // too many fields
            b64(&format!("{}:extra", plain(&checksum))),
            // bad uuid
            b64(&format!("nope:1700000000000:4:0:{checksum}")),
            // difficulty out of range
            b64(&format!("{}:1700000000000:0:0:{checksum}", session())),
            b64(&format!("{}:1700000000000:33:0:{checksum}", session())),
            // negative counter
            b64(&format!("{}:1700000000000:4:-1:{checksum}", session())),
            // timestamp beyond the legacy bound
            b64(&format!("{}:8640000000000001:4:0:{checksum}", session())),
            // checksum not 64 hex chars
            b64(&format!("{}:1700000000000:4:0:abc", session())),
        ];
        for case in cases {
            assert_eq!(
                Solution::parse_header(&case),
                Err(Rejection::Malformed),
                "expected malformed for {case}"
            );
        }
    }

    fn plain(checksum: &str) -> String {
        format!("{}:1700000000000:4:0:{checksum}", session())
    }

    fn b64(text: &str) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(text)
    }
}
