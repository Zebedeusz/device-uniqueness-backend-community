//! Android hardware-keystore (TEE) attestation for the vanilla/no-GMS app —
//! the `Auth-Attestation-Type: key-attestation` path on `POST /auth/token`.
//!
//! The shipping vanilla flavor mints a keystore key with
//! `setAttestationChallenge(challenge)` and sends the certificate chain
//! **inline in the request JSON body** as `attestationChain` (base64 DER
//! array, leaf first) — it is *not* a header. Verification is pure
//! ([`verify`]); the CRL fetch/cache ([`crl`]) is the only IO.

pub(crate) mod crl;
pub(crate) mod extension;
pub(crate) mod verify;

use base64::Engine as _;

/// Legacy body-contract bounds for `attestationChain`
/// (zod: `.min(2).max(10)`, each base64 entry `.max(8192)`).
const MIN_CHAIN_ENTRIES: usize = 2;
const MAX_CHAIN_ENTRIES: usize = 10;
const MAX_ENTRY_CHARS: usize = 8192;

/// Extract and decode the `attestationChain` field from the raw request body.
///
/// The caller keeps the raw bytes (the sr25519 proof hashes them), so this
/// parses the same bytes rather than a re-serialized body.
pub(crate) fn chain_from_body(body: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(rename = "attestationChain")]
        attestation_chain: Option<Vec<String>>,
    }

    let body: Body =
        serde_json::from_slice(body).map_err(|e| format!("body is not valid JSON: {e}"))?;
    let chain = body
        .attestation_chain
        .ok_or("body has no attestationChain field")?;
    if chain.len() < MIN_CHAIN_ENTRIES || chain.len() > MAX_CHAIN_ENTRIES {
        return Err(format!(
            "attestationChain has {} entries, expected {MIN_CHAIN_ENTRIES}..={MAX_CHAIN_ENTRIES}",
            chain.len()
        ));
    }

    let b64 = base64::engine::general_purpose::STANDARD;
    chain
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            if entry.len() > MAX_ENTRY_CHARS {
                return Err(format!(
                    "attestationChain[{i}] exceeds {MAX_ENTRY_CHARS} chars"
                ));
            }
            b64.decode(entry.trim())
                .map_err(|_| format!("attestationChain[{i}] is not valid base64"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_from_body_enforces_the_legacy_contract() {
        // Missing field / not JSON / wrong types are rejected.
        assert!(chain_from_body(b"{}").is_err());
        assert!(chain_from_body(b"not json").is_err());
        assert!(chain_from_body(br#"{"attestationChain": "AAAA"}"#).is_err());

        // Too short, too long, oversized entries, bad base64.
        assert!(chain_from_body(br#"{"attestationChain": ["AAAA"]}"#).is_err());
        let eleven = format!(
            r#"{{"attestationChain": [{}]}}"#,
            ["\"AAAA\""; 11].join(",")
        );
        assert!(chain_from_body(eleven.as_bytes()).is_err());
        let oversized = format!(
            r#"{{"attestationChain": ["{}", "AAAA"]}}"#,
            "A".repeat(8193)
        );
        assert!(chain_from_body(oversized.as_bytes()).is_err());
        assert!(chain_from_body(br#"{"attestationChain": ["!!!", "AAAA"]}"#).is_err());

        // A well-formed two-entry chain decodes leaf-first.
        let decoded = chain_from_body(br#"{"attestationChain": ["AQID", "BAUG"], "extra": 1}"#)
            .expect("valid body");
        assert_eq!(decoded, vec![vec![1, 2, 3], vec![4, 5, 6]]);
    }
}
