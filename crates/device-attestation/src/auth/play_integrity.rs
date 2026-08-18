//! Android Play Integrity — the `Auth-Attestation-Type: play-integrity` path
//! on `POST /auth/token`.
//!
//! The shipping gp flavor mints the token with
//! `setNonce(base64url(client_data_hash))` and sends it as the raw
//! `Auth-Payload` value. The classic
//! token is a JWE (`A256KW` + `A256GCM`) wrapping a JWS (`ES256`) verdict,
//! verified locally with the self-managed response keys ([`token`]) — no
//! Google API call. Until the Play Console key exchange lands, [`google`] is
//! a temporary `decodeIntegrityToken` fallback feeding the same policy;
//! delete it once the keys are deployed.

pub(crate) mod google;
pub(crate) mod token;
pub(crate) mod verdict;

/// Per-verdict acceptance mode, mirroring the legacy `PLAY_INTEGRITY_MODE`.
///
/// - `Strict` — production: Play-Store build on a hardware-backed,
///   non-rooted device, licensed.
/// - `RelaxedDevice` — nightly/paseo: relax device integrity only.
/// - `RelaxedAll` — preview/debug: additionally tolerate sideloaded builds
///   (`UNRECOGNIZED_VERSION` / `UNEVALUATED`, missing app-integrity fields,
///   `UNLICENSED`). A field that is present but mismatching is still
///   rejected in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayIntegrityMode {
    Strict,
    RelaxedDevice,
    RelaxedAll,
}

impl std::str::FromStr for PlayIntegrityMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "strict" => Ok(Self::Strict),
            "relaxed_device" => Ok(Self::RelaxedDevice),
            "relaxed_all" => Ok(Self::RelaxedAll),
            other => Err(format!(
                "{other:?} is not one of strict, relaxed_device, relaxed_all"
            )),
        }
    }
}

/// Why a Play Integrity token was rejected. Safe to log; callers map every
/// variant onto the same posture-gated verdict failure.
#[derive(Debug, thiserror::Error)]
pub enum PlayIntegrityError {
    #[error("malformed integrity token: {0}")]
    Malformed(String),
    #[error("integrity token decryption failed: {0}")]
    Decrypt(String),
    #[error("integrity token signature invalid")]
    Signature,
    #[error("integrity token nonce mismatch")]
    Nonce,
    #[error("integrity verdicts rejected: {}", .0.join(","))]
    Rejected(Vec<&'static str>),
}

/// Everything needed to verify one token besides the token itself.
pub struct VerifyParams<'a> {
    /// AES-256 response decryption key (`DECRYPTION_KEY`).
    pub decryption_key: &'a [u8; 32],
    /// EC P-256 verification key, DER SPKI (`VERIFICATION_KEY`).
    pub verification_key_der: &'a [u8],
    /// Request-binding and verdict policy.
    pub policy: PolicyParams<'a>,
}

/// Request binding + verdict policy, shared by the self-managed path and the
/// temporary Google-API fallback (which only replaces the decode step).
pub struct PolicyParams<'a> {
    /// Expected nonce bytes: the shared `client_data_hash` for this request.
    pub expected_nonce: &'a [u8; 32],
    /// Verdict acceptance mode.
    pub mode: PlayIntegrityMode,
    /// Allow-listed Android package names.
    pub package_names: &'a [String],
    /// SHA-256 digest of the Play Store signing certificate.
    pub playstore_digest: &'a [u8; 32],
    /// SHA-256 digest of the website/vanilla-APK signing certificate.
    pub website_digest: &'a [u8; 32],
}

/// Decrypt, verify, and policy-check a token with the self-managed keys.
/// Returns `appFromOfficialStore` (Play digest → `true`, website → `false`).
pub fn verify_token(
    integrity_token: &str,
    params: &VerifyParams<'_>,
) -> Result<bool, PlayIntegrityError> {
    let payload = token::decrypt_and_verify(
        integrity_token,
        params.decryption_key,
        params.verification_key_der,
    )?;
    check_payload(&payload, &params.policy)
}

/// Bind a decoded verdict payload to this request (nonce) and policy-check
/// it. Shared by both decoders.
pub fn check_payload(
    payload: &token::TokenPayload,
    policy: &PolicyParams<'_>,
) -> Result<bool, PlayIntegrityError> {
    // The client set the nonce to the same hash the sr25519 proof signs.
    let nonce = payload
        .request_details
        .as_ref()
        .and_then(|d| d.nonce.as_deref())
        .ok_or(PlayIntegrityError::Nonce)?;
    let nonce_bytes = token::b64url(nonce).map_err(|_| PlayIntegrityError::Nonce)?;
    if nonce_bytes != policy.expected_nonce {
        return Err(PlayIntegrityError::Nonce);
    }

    verdict::validate(payload, policy)
}

#[cfg(test)]
mod tests;
