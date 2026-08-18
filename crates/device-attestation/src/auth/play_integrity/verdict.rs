//! Play Integrity verdict policy — a straight port of the legacy
//! `play-integrity/validation.workflow.ts` mode tables and error codes.
//! Every check runs and all failures are collected (legacy behavior).

use super::token::TokenPayload;
use super::{PlayIntegrityError, PlayIntegrityMode, PolicyParams};

/// Legacy error codes, logged joined with `,` on rejection.
pub const APP_INTEGRITY_FAILED: &str = "APP_INTEGRITY_FAILED";
pub const DEVICE_INTEGRITY_FAILED: &str = "DEVICE_INTEGRITY_FAILED";
pub const LICENSE_CHECK_FAILED: &str = "LICENSE_CHECK_FAILED";
pub const APK_FINGERPRINT_MISMATCH: &str = "APK_FINGERPRINT_MISMATCH";
pub const PACKAGE_NAME_MISMATCH: &str = "PACKAGE_NAME_MISMATCH";

/// Per-mode acceptance rules (exact legacy tables).
struct ModePolicy {
    accept_app_recognition: &'static [&'static str],
    accept_device_recognition: &'static [&'static str],
    accept_empty_device_recognition: bool,
    accept_app_licensing: &'static [&'static str],
    /// Also tolerates missing `packageName` / `certificateSha256Digest`
    /// (Google omits them when `appRecognitionVerdict == UNEVALUATED`).
    accept_missing_app_integrity: bool,
}

const STRICT: ModePolicy = ModePolicy {
    accept_app_recognition: &["PLAY_RECOGNIZED"],
    accept_device_recognition: &["MEETS_STRONG_INTEGRITY", "MEETS_DEVICE_INTEGRITY"],
    accept_empty_device_recognition: false,
    accept_app_licensing: &["LICENSED"],
    accept_missing_app_integrity: false,
};

const RELAXED_DEVICE: ModePolicy = ModePolicy {
    accept_app_recognition: &["PLAY_RECOGNIZED"],
    accept_device_recognition: &[
        "MEETS_DEVICE_INTEGRITY",
        "MEETS_BASIC_INTEGRITY",
        "MEETS_STRONG_INTEGRITY",
    ],
    accept_empty_device_recognition: false,
    accept_app_licensing: &["LICENSED"],
    accept_missing_app_integrity: false,
};

const RELAXED_ALL: ModePolicy = ModePolicy {
    accept_app_recognition: &["PLAY_RECOGNIZED", "UNRECOGNIZED_VERSION", "UNEVALUATED"],
    accept_device_recognition: &[
        "MEETS_DEVICE_INTEGRITY",
        "MEETS_BASIC_INTEGRITY",
        "MEETS_STRONG_INTEGRITY",
        "MEETS_VIRTUAL_INTEGRITY",
    ],
    accept_empty_device_recognition: true,
    accept_app_licensing: &["LICENSED", "UNLICENSED", "UNEVALUATED"],
    accept_missing_app_integrity: true,
};

/// Known enum values per field: the legacy ACL maps unknown wire strings to
/// null (i.e. "missing"), never to an accepted value.
const KNOWN_APP_RECOGNITION: &[&str] = &["PLAY_RECOGNIZED", "UNRECOGNIZED_VERSION", "UNEVALUATED"];
const KNOWN_DEVICE_RECOGNITION: &[&str] = &[
    "MEETS_DEVICE_INTEGRITY",
    "MEETS_BASIC_INTEGRITY",
    "MEETS_STRONG_INTEGRITY",
    "MEETS_VIRTUAL_INTEGRITY",
];
const KNOWN_APP_LICENSING: &[&str] = &["LICENSED", "UNLICENSED", "UNEVALUATED"];

fn policy(mode: PlayIntegrityMode) -> &'static ModePolicy {
    match mode {
        PlayIntegrityMode::Strict => &STRICT,
        PlayIntegrityMode::RelaxedDevice => &RELAXED_DEVICE,
        PlayIntegrityMode::RelaxedAll => &RELAXED_ALL,
    }
}

/// Normalize a verdict string: unknown values become `None` like the legacy ACL.
fn known<'a>(value: Option<&'a str>, known_values: &[&str]) -> Option<&'a str> {
    value.filter(|v| known_values.contains(v))
}

/// Validate the verdict payload against the mode policy; on success derive
/// `appFromOfficialStore` from the signing-digest routing.
pub fn validate(
    payload: &TokenPayload,
    params: &PolicyParams<'_>,
) -> Result<bool, PlayIntegrityError> {
    let policy = policy(params.mode);
    let mut codes: Vec<&'static str> = Vec::new();

    // appRecognitionVerdict — null accepted only if the policy lists null
    // (no policy does; RELAXED_ALL accepts UNEVALUATED, which is a value).
    let app_recognition = known(
        payload
            .app_integrity
            .as_ref()
            .and_then(|a| a.app_recognition_verdict.as_deref()),
        KNOWN_APP_RECOGNITION,
    );
    match app_recognition {
        Some(verdict) if policy.accept_app_recognition.contains(&verdict) => {}
        _ => codes.push(APP_INTEGRITY_FAILED),
    }

    let device_verdicts: Vec<&str> = payload
        .device_integrity
        .as_ref()
        .and_then(|d| d.device_recognition_verdict.as_ref())
        .map(|list| {
            list.iter()
                .map(String::as_str)
                .filter(|v| KNOWN_DEVICE_RECOGNITION.contains(v))
                .collect()
        })
        .unwrap_or_default();
    let device_ok = if device_verdicts.is_empty() {
        policy.accept_empty_device_recognition
    } else {
        device_verdicts
            .iter()
            .any(|v| policy.accept_device_recognition.contains(v))
    };
    if !device_ok {
        codes.push(DEVICE_INTEGRITY_FAILED);
    }

    // appLicensingVerdict.
    let licensing = known(
        payload
            .account_details
            .as_ref()
            .and_then(|a| a.app_licensing_verdict.as_deref()),
        KNOWN_APP_LICENSING,
    );
    match licensing {
        Some(verdict) if policy.accept_app_licensing.contains(&verdict) => {}
        _ => codes.push(LICENSE_CHECK_FAILED),
    }

    // Token digests are base64url; compare decoded bytes.
    let token_digests: Vec<Vec<u8>> = payload
        .app_integrity
        .as_ref()
        .and_then(|a| a.certificate_sha256_digest.as_ref())
        .map(|list| {
            list.iter()
                .filter_map(|d| super::token::b64url(d).ok())
                .collect()
        })
        .unwrap_or_default();
    let matches_expected = |digest: &[u8]| {
        digest == params.playstore_digest.as_slice() || digest == params.website_digest.as_slice()
    };
    if token_digests.is_empty() {
        if !policy.accept_missing_app_integrity {
            codes.push(APK_FINGERPRINT_MISMATCH);
        }
    } else if !token_digests.iter().any(|d| matches_expected(d)) {
        codes.push(APK_FINGERPRINT_MISMATCH);
    }

    // packageName.
    let package_name = payload
        .app_integrity
        .as_ref()
        .and_then(|a| a.package_name.as_deref());
    match package_name {
        Some(name) => {
            if !params.package_names.iter().any(|p| p == name) {
                codes.push(PACKAGE_NAME_MISMATCH);
            }
        }
        None => {
            if !policy.accept_missing_app_integrity {
                codes.push(PACKAGE_NAME_MISMATCH);
            }
        }
    }

    if !codes.is_empty() {
        return Err(PlayIntegrityError::Rejected(codes));
    }

    // appFromOfficialStore: Play Store digest → true; website digest →
    // false; digests absent (tolerated only in relaxed_all) → not verified
    // from the store → false.
    Ok(token_digests
        .iter()
        .any(|d| d == params.playstore_digest.as_slice()))
}
