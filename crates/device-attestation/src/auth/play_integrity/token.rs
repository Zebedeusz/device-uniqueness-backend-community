//! Classic Play Integrity token crypto: JWE (`A256KW` + `A256GCM`) → JWS
//! (`ES256`) → typed JSON verdict payload. Pure — callers supply the keys.

use aes_gcm::aead::{Aead as _, Payload};
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
use base64::Engine as _;
use p256::ecdsa::signature::hazmat::PrehashVerifier as _;
use p256::pkcs8::DecodePublicKey as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::PlayIntegrityError;

/// Decode base64url input, tolerating both padded and unpadded forms.
pub(crate) fn b64url(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let trimmed = input.trim();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(trimmed))
}

/// The verdict fields this service validates, in Google's wire shape.
/// Unknown enum values are kept as strings and mapped to "unrecognized" by
/// the verdict policy (the legacy ACL drops them to null the same way).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPayload {
    pub request_details: Option<RequestDetails>,
    pub app_integrity: Option<AppIntegrity>,
    pub device_integrity: Option<DeviceIntegrity>,
    pub account_details: Option<AccountDetails>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestDetails {
    pub nonce: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppIntegrity {
    pub app_recognition_verdict: Option<String>,
    pub package_name: Option<String>,
    pub certificate_sha256_digest: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIntegrity {
    pub device_recognition_verdict: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDetails {
    pub app_licensing_verdict: Option<String>,
}

/// Decrypt the JWE with the AES-256 decryption key, verify the inner JWS
/// with the EC P-256 verification key, and parse the verdict payload.
pub fn decrypt_and_verify(
    integrity_token: &str,
    decryption_key: &[u8; 32],
    verification_key_der: &[u8],
) -> Result<TokenPayload, PlayIntegrityError> {
    let jws = decrypt_jwe(integrity_token, decryption_key)?;
    let payload = verify_jws(&jws, verification_key_der)?;
    serde_json::from_slice(&payload)
        .map_err(|e| PlayIntegrityError::Malformed(format!("verdict payload: {e}")))
}

/// Decrypt the outer JWE (`A256KW` key wrap + `A256GCM` content encryption),
/// returning the plaintext (the inner JWS compact serialization).
fn decrypt_jwe(token: &str, decryption_key: &[u8; 32]) -> Result<Vec<u8>, PlayIntegrityError> {
    let malformed = |what: &str| PlayIntegrityError::Malformed(format!("jwe: {what}"));

    let parts: Vec<&str> = token.trim().split('.').collect();
    let [header_b64, encrypted_key, iv, ciphertext, tag] = parts.as_slice() else {
        return Err(malformed("expected 5 dot-separated parts"));
    };

    #[derive(Deserialize)]
    struct JweHeader {
        alg: String,
        enc: String,
    }
    let header: JweHeader =
        serde_json::from_slice(&b64url(header_b64).map_err(|_| malformed("header base64"))?)
            .map_err(|_| malformed("header json"))?;
    if header.alg != "A256KW" || header.enc != "A256GCM" {
        return Err(malformed(&format!(
            "unsupported alg/enc {}/{}",
            header.alg, header.enc
        )));
    }

    let wrapped_cek = b64url(encrypted_key).map_err(|_| malformed("encrypted key base64"))?;
    let kek = aes_kw::KekAes256::from(*decryption_key);
    let cek = kek
        .unwrap_vec(&wrapped_cek)
        .map_err(|e| PlayIntegrityError::Decrypt(format!("key unwrap: {e}")))?;

    let iv = b64url(iv).map_err(|_| malformed("iv base64"))?;
    if iv.len() != 12 {
        return Err(malformed("iv is not 96 bits"));
    }
    let mut ciphertext_and_tag = b64url(ciphertext).map_err(|_| malformed("ciphertext base64"))?;
    ciphertext_and_tag.extend(b64url(tag).map_err(|_| malformed("tag base64"))?);

    let cipher = Aes256Gcm::new_from_slice(&cek)
        .map_err(|_| PlayIntegrityError::Decrypt("content key is not 256 bits".into()))?;
    cipher
        .decrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &ciphertext_and_tag,
                // JWE integrity binds the protected header as AAD.
                aad: header_b64.as_bytes(),
            },
        )
        .map_err(|_| PlayIntegrityError::Decrypt("authenticated decryption failed".into()))
}

/// Verify the inner JWS (`ES256` over the signing input, raw `r||s`
/// signature), returning the payload bytes.
fn verify_jws(jws: &[u8], verification_key_der: &[u8]) -> Result<Vec<u8>, PlayIntegrityError> {
    let malformed = |what: &str| PlayIntegrityError::Malformed(format!("jws: {what}"));

    let jws = std::str::from_utf8(jws).map_err(|_| malformed("not utf-8"))?;
    let parts: Vec<&str> = jws.trim().split('.').collect();
    let [header_b64, payload_b64, signature_b64] = parts.as_slice() else {
        return Err(malformed("expected 3 dot-separated parts"));
    };

    #[derive(Deserialize)]
    struct JwsHeader {
        alg: String,
    }
    let header: JwsHeader =
        serde_json::from_slice(&b64url(header_b64).map_err(|_| malformed("header base64"))?)
            .map_err(|_| malformed("header json"))?;
    if header.alg != "ES256" {
        return Err(malformed(&format!("unsupported alg {}", header.alg)));
    }

    let key = p256::ecdsa::VerifyingKey::from_public_key_der(verification_key_der)
        .map_err(|e| PlayIntegrityError::Malformed(format!("verification key: {e}")))?;
    let signature_bytes = b64url(signature_b64).map_err(|_| malformed("signature base64"))?;
    let signature = p256::ecdsa::Signature::from_slice(&signature_bytes)
        .map_err(|_| PlayIntegrityError::Signature)?;

    let signing_input = format!("{header_b64}.{payload_b64}");
    let prehash = Sha256::digest(signing_input.as_bytes());
    key.verify_prehash(&prehash, &signature)
        .map_err(|_| PlayIntegrityError::Signature)?;

    b64url(payload_b64).map_err(|_| malformed("payload base64"))
}
