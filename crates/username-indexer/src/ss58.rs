//! SS58 encoding for People Chain account identifiers.

use blake2::{Blake2b512, Digest as _};

/// SS58 encoding failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Ss58Error {
    /// SS58 supports network identifiers below 16,384.
    #[error("SS58 prefix {0} is outside the supported range")]
    InvalidPrefix(u16),
}

/// Reject a chain SS58 prefix [`encode`] cannot represent (>= 16,384) before a
/// scan starts, so ingest fails fast once instead of erroring on every row.
/// Shared by the bootstrap and incremental ingest paths.
pub(crate) fn validate_prefix(prefix: u16) -> Result<u16, Ss58Error> {
    if prefix >= 16_384 {
        Err(Ss58Error::InvalidPrefix(prefix))
    } else {
        Ok(prefix)
    }
}

/// Encode a 32-byte account identifier using the supplied chain SS58 prefix.
pub fn encode(account_id: &[u8; 32], prefix: u16) -> Result<String, Ss58Error> {
    let mut payload = Vec::with_capacity(36);
    match prefix {
        0..=63 => payload.push(prefix as u8),
        64..=16_383 => {
            payload.push(((prefix & 0b00_11111100) >> 2) as u8 | 0b0100_0000);
            payload.push((prefix >> 8) as u8 | ((prefix & 0b11) << 6) as u8);
        }
        _ => return Err(Ss58Error::InvalidPrefix(prefix)),
    }
    payload.extend_from_slice(account_id);

    let mut checksum_input = b"SS58PRE".to_vec();
    checksum_input.extend_from_slice(&payload);
    let checksum = Blake2b512::digest(checksum_input);
    payload.extend_from_slice(&checksum[..2]);
    Ok(bs58::encode(payload).into_string())
}

#[cfg(test)]
mod tests {
    use super::{encode, validate_prefix, Ss58Error};

    /// The prefix gate matches `encode`'s own range exactly: 16,383 is the
    /// last encodable prefix, 16,384 the first rejected one.
    #[test]
    fn validate_prefix_shares_encodes_boundary() {
        assert_eq!(validate_prefix(0), Ok(0));
        assert_eq!(validate_prefix(16_383), Ok(16_383));
        assert!(encode(&[0u8; 32], 16_383).is_ok());
        assert_eq!(
            validate_prefix(16_384),
            Err(Ss58Error::InvalidPrefix(16_384))
        );
        assert_eq!(
            encode(&[0u8; 32], 16_384),
            Err(Ss58Error::InvalidPrefix(16_384))
        );
    }

    #[test]
    fn encodes_known_substrate_alice_address() {
        let account = [
            0xd4, 0x35, 0x93, 0xc7, 0x15, 0xfd, 0xd3, 0x1c, 0x61, 0x14, 0x1a, 0xbd, 0x04, 0xa9,
            0x9f, 0xd6, 0x82, 0x2c, 0x85, 0x58, 0x85, 0x4c, 0xcd, 0xe3, 0x9a, 0x56, 0x84, 0xe7,
            0xa5, 0x6d, 0xa2, 0x7d,
        ];
        assert_eq!(
            encode(&account, 42).expect("valid prefix"),
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"
        );
    }
}
