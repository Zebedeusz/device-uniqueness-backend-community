//! Live APNs auth/connectivity smoke — ignored by default.
//!
//! Sends to a dummy device token and asserts the failure is a device-token error,
//! not auth/config — i.e. Apple accepted the ES256 token, topic, and HTTP/2. A
//! `.p8` is valid for both environments, so sandbox confirms production auth too.
//!
//! Run manually with real credentials in the environment (never committed):
//!
//! ```sh
//! APNS_PRIVATE_KEY="$(cat AuthKey_XXXX.p8)" APNS_KEY_ID=... APNS_TEAM_ID=... \
//!   APNS_TOPIC=io.parity.brevity APNS_ENVIRONMENT=development \
//!   cargo test -p notifications --test apns_live -- --ignored --nocapture
//! ```

use notifications::{ApnsConfig, ApnsProvider, PushProvider, PushRequest};

/// APNs reasons that mean the provider token / topic / config is wrong.
const AUTH_ERROR_REASONS: &[&str] = &[
    "InvalidProviderToken",
    "ExpiredProviderToken",
    "MissingProviderToken",
    "InvalidProviderTokenSignature",
    "MissingTopic",
    "TopicDisallowed",
    "BadCertificate",
    "BadCertificateEnvironment",
    "Forbidden",
];

#[tokio::test]
#[ignore = "hits Apple's live APNs servers; needs real APNS_* env"]
async fn apns_auth_and_connectivity_smoke() {
    let Some(config) = ApnsConfig::from_env().expect("valid APNS_* config") else {
        eprintln!("APNS_* not configured; skipping live smoke");
        return;
    };
    let environment = config.environment;
    let provider = ApnsProvider::new(config).expect("build APNs provider");

    // Well-formed but non-existent device token: Apple must reject the token,
    // not the provider auth.
    let request = PushRequest {
        device_token: "0".repeat(64),
        push_id: "5d41402abc4b2a76b9719d911017c592".to_string(),
        message: "0xdeadbeef".to_string(),
        topic: None,
        voip: Some(false),
    };

    let outcome = provider
        .send(&request)
        .await
        .expect("APNs request should reach Apple (transport OK)");

    if outcome.success {
        eprintln!("APNs accepted the push (unexpected for a dummy token, but auth is valid)");
        return;
    }

    let error = outcome
        .errors
        .as_ref()
        .and_then(|errors| errors.first())
        .expect("a failure should carry an error");
    let reason = error
        .response
        .as_ref()
        .and_then(|value| value.get("reason"))
        .and_then(|value| value.as_str())
        .unwrap_or("<none>");
    eprintln!(
        "APNs env={environment:?} status={:?} reason={reason} — provider token accepted, token rejected as expected",
        error.status
    );

    assert!(
        !AUTH_ERROR_REASONS.contains(&reason),
        "APNs rejected the provider auth/config (reason={reason}); check key, key id, team id, topic, environment"
    );
}
