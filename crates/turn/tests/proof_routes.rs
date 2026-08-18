//! Proof-route integration tests (`docs/plans/done/personhood-proof-turn.md`)
//! using the production router and a statically seeded test ring.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;
use turn::config::{Config, ProofConfig, PROOF_MAX_SKEW_SECS};
use turn::credentials::Algorithm;
use turn::proof::message::Freshness;
use turn::proof::roots::{AcceptedRoot, PersonhoodCollection, Snapshot};
use turn::AppState;
use verifiable::ring::ark_vrf::ring::SrsLookup as _;
use verifiable::ring::bandersnatch::{BandersnatchProverCache, BandersnatchVrfVerifiable};
use verifiable::ring::{ProverCache as _, RingDomainSize, StaticChunk};
use verifiable::GenerateVerifiable as _;

type Vrf = BandersnatchVrfVerifiable;
type Secret = <Vrf as verifiable::GenerateVerifiable>::Secret;
type Member = <Vrf as verifiable::GenerateVerifiable>::Member;
type Members = <Vrf as verifiable::GenerateVerifiable>::Members;

const DOMAIN: RingDomainSize = RingDomainSize::Domain11;
const GENESIS: [u8; 32] = [7u8; 32];
const PRODUCT: &str = "test-product.dot";
const OTHER_PRODUCT: &str = "other-product.dot";
const PEOPLE_LITE_COLLECTION: &str =
    "0x706f703a706f6c6b61646f742e6e6574776f726b2f70656f706c652d6c697465";
const PEOPLE_COLLECTION: &str =
    "0x706f703a706f6c6b61646f742e6e6574776f726b2f70656f706c652020202020";
const SUFFIX: u32 = 0;
/// The revision the seeded test root commits to.
const REVISION: u32 = 4;

/// The context the server derives for `PRODUCT` — the one a host would prove
/// under when `PRODUCT` is the calling product.
fn context() -> [u8; 32] {
    turn::proof::context::product_context(PRODUCT, SUFFIX)
}

fn build_ring(members: &[Member]) -> Members {
    let setup = BandersnatchProverCache::ring_setup(DOMAIN);
    let (_, pcs_params) = setup.verifier_key_builder();
    let mut intermediate = Vrf::start_members(DOMAIN);
    Vrf::push_members(&mut intermediate, members.iter().cloned(), |range| {
        (&pcs_params)
            .lookup(range)
            .map(|points| points.into_iter().map(StaticChunk).collect())
            .ok_or(())
    })
    .expect("members fit the ring");
    Vrf::finish_members(intermediate)
}

struct TestRing {
    secrets: Vec<Secret>,
    members: Vec<Member>,
    commitment: Members,
}

fn test_ring() -> TestRing {
    let secrets: Vec<_> = (0u8..4).map(|i| Vrf::new_secret([i; 32])).collect();
    let members: Vec<_> = secrets.iter().map(Vrf::member_from_secret).collect();
    let commitment = build_ring(&members);
    TestRing {
        secrets,
        members,
        commitment,
    }
}

fn proof_config() -> ProofConfig {
    ProofConfig {
        rpc_url: "ws://unused.invalid".to_string(),
        genesis: GENESIS,
        contexts: [PRODUCT, OTHER_PRODUCT]
            .into_iter()
            .map(|product| {
                (
                    product.to_string(),
                    turn::proof::context::product_context(product, SUFFIX),
                )
            })
            .collect(),
        concurrency: 2,
    }
}

/// Production router with proof enabled and the given snapshot seeded (when
/// `Some`); `proof: None` builds the feature-off router.
fn app(proof: Option<(ProofConfig, Option<Snapshot>)>) -> axum::Router {
    app_with_rate_limit(proof, 100)
}

fn app_with_rate_limit(
    proof: Option<(ProofConfig, Option<Snapshot>)>,
    rate_limit: u32,
) -> axum::Router {
    turn::routes(state_with_rate_limit(proof, rate_limit))
}

fn state_with_rate_limit(
    proof: Option<(ProofConfig, Option<Snapshot>)>,
    rate_limit: u32,
) -> AppState {
    let key = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
    let (proof_config, snapshot) = match proof {
        Some((config, snapshot)) => (Some(config), snapshot),
        None => (None, None),
    };
    let config = Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        turn_secret: b"integration-turn-secret".to_vec(),
        algorithm: Algorithm::Sha1,
        ttl_secs: 1800,
        realm: "example.org".to_string(),
        ice_servers: vec!["turn:turn.example.org:3478?transport=udp".to_string()],
        jwt_verifier: jwt_verify::Verifier::from_verifying_key(key.verifying_key()),
        rate_limit,
        rate_window: Duration::from_secs(60),
        proof: proof_config,
    };
    let state = AppState::new(config);
    if let (Some(proof_state), Some(snapshot)) = (&state.proof, snapshot) {
        proof_state
            .roots
            .get(PersonhoodCollection::PeopleLite)
            .set(snapshot);
    }
    state
}

fn snapshot_of(commitment: Members) -> Snapshot {
    Snapshot {
        domain: DOMAIN,
        roots: std::sync::Arc::new(vec![AcceptedRoot {
            ring_index: 0,
            revision: REVISION,
            members: commitment,
        }]),
    }
}

async fn post_json(
    app: &axum::Router,
    path: &str,
    body: Option<String>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method("POST").uri(path);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json)
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::String(
            String::from_utf8_lossy(&bytes).to_string(),
        ))
    };
    (status, json)
}

/// Create a proof over the message the server derives for `timestamp`.
fn prove(ring: &TestRing, secret_index: usize, context: &[u8], timestamp: u64) -> String {
    let message = Freshness::new(PROOF_MAX_SKEW_SECS).message(timestamp);
    let member = &ring.members[secret_index];
    let opening = Vrf::open(DOMAIN, member, ring.members.iter().cloned()).expect("member in ring");
    let (proof, _alias) =
        Vrf::create(opening, &ring.secrets[secret_index], context, &message).expect("proof");
    format!("0x{}", hex::encode(&proof))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn body(proof: &str, ring_index: u32, timestamp: u64) -> String {
    body_for(PRODUCT, proof, ring_index, timestamp)
}

fn body_with_revision(proof: &str, ring_index: u32, ring_revision: u32, timestamp: u64) -> String {
    format!(
        r#"{{"productId":"{PRODUCT}","collection":"{PEOPLE_LITE_COLLECTION}","proof":"{proof}","ringIndex":{ring_index},"ringRevision":{ring_revision},"timestamp":{timestamp}}}"#
    )
}

fn body_for(product: &str, proof: &str, ring_index: u32, timestamp: u64) -> String {
    format!(
        r#"{{"productId":"{product}","collection":"{PEOPLE_LITE_COLLECTION}","proof":"{proof}","ringIndex":{ring_index},"ringRevision":{REVISION},"timestamp":{timestamp}}}"#
    )
}

/// Prove and redeem in one step with a fresh timestamp.
fn fresh_body(ring: &TestRing, secret_index: usize) -> String {
    // Give the expensive prover the full symmetric freshness budget so this
    // remains stable when the workspace runs every ring test in parallel.
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(ring, secret_index, &context(), timestamp);
    body(&proof, 0, timestamp)
}

#[tokio::test]
async fn proof_flow_mints_credentials_and_rejects_bad_proofs() {
    let ring = test_ring();
    let app = app(Some((
        proof_config(),
        Some(snapshot_of(ring.commitment.clone())),
    )));

    // Happy path: one request, no challenge, no Authorization header.
    let (status, json) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 2)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
    assert!(json["username"].as_str().is_some_and(|u| u.contains(':')));
    assert!(json["password"].as_str().is_some());
    assert_eq!(json["ttl"], serde_json::json!(1800));
    // No alias or collection leaks into the body.
    let mut keys: Vec<_> = json.as_object().expect("object").keys().collect();
    keys.sort();
    assert_eq!(keys, ["password", "servers", "ttl", "username"]);

    // A tampered proof is rejected (403), deliberately unspecific.
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 2, &context(), timestamp);
    let mut bytes = hex::decode(proof.trim_start_matches("0x")).expect("hex");
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let tampered = format!("0x{}", hex::encode(bytes));
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body(&tampered, 0, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Only the two canonical personhood collections are accepted.
    let invalid_collection = body(&proof, 0, timestamp).replace(
        PEOPLE_LITE_COLLECTION,
        "0x0101010101010101010101010101010101010101010101010101010101010101",
    );
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(invalid_collection),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A proof made under another accepted product's context cannot be
    // redeemed as this one.
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(
        &ring,
        2,
        &turn::proof::context::product_context(OTHER_PRODUCT, SUFFIX),
        timestamp,
    );
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body(&proof, 0, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A proof over different bytes than the server derives is rejected: both
    // timestamps sit inside the skew window, so this fails on the digest and
    // not on freshness.
    let proof = prove(&ring, 2, &context(), timestamp - 5);
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body(&proof, 0, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A timestamp outside the accepted skew is rejected before verification.
    let stale = now_unix() - PROOF_MAX_SKEW_SECS - 1;
    let proof = prove(&ring, 2, &context(), stale);
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body(&proof, 0, stale)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 2, &context(), timestamp);

    // Unparseable body → 400.
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some("{nope".to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // ringIndex is required and limits verification to that ring.
    let missing =
        format!(r#"{{"productId":"{PRODUCT}","proof":"{proof}","timestamp":{timestamp}}}"#);
    let (status, _) = post_json(&app, "/api/v1/turn/issue-with-proof", Some(missing)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A product this deployment does not accept is refused before any
    // verification work.
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body_for("unlisted.dot", &proof, 0, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body(&proof, 1, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_named_ring_revision_selects_exactly_that_root() {
    // Two revisions of ring 0 are held: the current one, and the previous
    // one whose ring the prover is not a member of.
    let ring = test_ring();
    let stale_members: Vec<_> = (10u8..14)
        .map(|i| Vrf::member_from_secret(&Vrf::new_secret([i; 32])))
        .collect();
    let snapshot = Snapshot {
        domain: DOMAIN,
        roots: std::sync::Arc::new(vec![
            AcceptedRoot {
                ring_index: 0,
                revision: REVISION,
                members: ring.commitment.clone(),
            },
            AcceptedRoot {
                ring_index: 0,
                revision: REVISION - 1,
                members: build_ring(&stale_members),
            },
        ]),
    };
    let app = app(Some((proof_config(), Some(snapshot))));

    // The current revision: the root it names verifies.
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 3, &context(), timestamp);
    let (status, json) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body_with_revision(&proof, 0, REVISION, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{json}");

    // Naming the previous revision restricts verification to that root
    // alone — it cannot fall through to the current one that would verify.
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 0, &context(), timestamp);
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body_with_revision(&proof, 0, REVISION - 1, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A revision outside the roots we still hold verifies against nothing.
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 1, &context(), timestamp);
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body_with_revision(&proof, 0, REVISION - 2, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_missing_ring_revision_is_refused_before_verification() {
    let ring = test_ring();
    let app = app(Some((
        proof_config(),
        Some(snapshot_of(ring.commitment.clone())),
    )));

    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 0, &context(), timestamp);
    let body = format!(
        r#"{{"productId":"{PRODUCT}","proof":"{proof}","ringIndex":0,"timestamp":{timestamp}}}"#
    );

    let (status, _) = post_json(&app, "/api/v1/turn/issue-with-proof", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn proofs_are_raw_host_bytes_of_exactly_one_signature() {
    use subxt::ext::codec::Encode as _;

    let ring = test_ring();
    let app = app(Some((
        proof_config(),
        Some(snapshot_of(ring.commitment.clone())),
    )));

    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let message = Freshness::new(PROOF_MAX_SKEW_SECS).message(timestamp);
    let opening =
        Vrf::open(DOMAIN, &ring.members[2], ring.members.iter().cloned()).expect("member in ring");
    let (proof, _) = Vrf::create(opening, &ring.secrets[2], &context(), &message).expect("proof");
    let raw = proof.to_vec();

    // Hosts return `proof.to_vec()`, so the wire form carries no SCALE
    // compact length prefix. A prefixed proof is the wrong length and is
    // refused before any verification work.
    for (label, bytes) in [
        ("scale-prefixed", proof.encode()),
        ("truncated", raw[..raw.len() - 1].to_vec()),
        ("overlong", [raw.clone(), vec![0u8]].concat()),
        ("empty", Vec::new()),
    ] {
        let (status, _) = post_json(
            &app,
            "/api/v1/turn/issue-with-proof",
            Some(body(&format!("0x{}", hex::encode(&bytes)), 0, timestamp)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}");
    }

    // The same proof, raw, is accepted.
    let (status, json) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body(&format!("0x{}", hex::encode(&proof)), 0, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
}

#[tokio::test]
async fn each_request_mints_full_ttl_credentials() {
    let ring = test_ring();
    let app = app(Some((
        proof_config(),
        Some(snapshot_of(ring.commitment.clone())),
    )));

    let request = fresh_body(&ring, 1);
    let (status, json) = post_json(&app, "/api/v1/turn/issue-with-proof", Some(request)).await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
    assert_eq!(json["ttl"], serde_json::json!(1800));
    let username = json["username"].as_str().expect("username");
    assert_eq!(username.split(':').nth(1).expect("id half").len(), 32);
}

#[tokio::test]
async fn each_canonical_collection_selects_only_its_own_cache() {
    let ring = test_ring();
    let state = state_with_rate_limit(Some((proof_config(), None)), 100);
    let proof_state = state.proof.as_ref().expect("proof enabled");
    proof_state
        .roots
        .get(PersonhoodCollection::People)
        .set(snapshot_of(ring.commitment.clone()));
    let app = turn::routes(state);

    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 1, &context(), timestamp);
    let request = body(&proof, 0, timestamp).replace(PEOPLE_LITE_COLLECTION, PEOPLE_COLLECTION);
    let (status, json) = post_json(&app, "/api/v1/turn/issue-with-proof", Some(request)).await;
    assert_eq!(status, StatusCode::CREATED, "{json}");

    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body(&proof, 0, now_unix())),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn alias_throttle_limits_the_same_person() {
    let ring = test_ring();
    let app = app_with_rate_limit(
        Some((proof_config(), Some(snapshot_of(ring.commitment.clone())))),
        1,
    );

    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The same person again: the alias bucket is exhausted.
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // A different person still succeeds.
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 3)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn each_product_gets_its_own_budget_for_the_same_person() {
    let ring = test_ring();
    let app = app_with_rate_limit(
        Some((proof_config(), Some(snapshot_of(ring.commitment.clone())))),
        1,
    );

    // Exhaust this person's budget under the first product.
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // The same member key under another product's context yields a different
    // alias, so it carries its own budget — the cost of not prompting.
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(
        &ring,
        1,
        &turn::proof::context::product_context(OTHER_PRODUCT, SUFFIX),
        timestamp,
    );
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body_for(OTHER_PRODUCT, &proof, 0, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_stale_root_snapshot_fails_closed() {
    let ring = test_ring();
    // A snapshot older than the max age is the same as having none: a member
    // removed from the ring must stop verifying in bounded time, even while the
    // RPC that would refresh it stays unreachable.
    let state = state_with_rate_limit(Some((proof_config(), None)), 100);
    let proof_state = state.proof.as_ref().expect("proof enabled");
    let roots = proof_state.roots.get(PersonhoodCollection::PeopleLite);
    roots.set(snapshot_of(ring.commitment.clone()));

    assert!(roots.snapshot(std::time::Duration::ZERO).is_none());
    assert!(roots.snapshot(turn::config::PROOF_MAX_ROOT_AGE).is_some());
}

#[tokio::test]
async fn no_root_snapshot_means_unavailable_not_broken() {
    let ring = test_ring();
    // Proof enabled but the cache never primed (chain unreachable since boot).
    let app = app(Some((proof_config(), None)));

    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 0)),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn saturated_verification_waits_for_a_slot() {
    let ring = test_ring();
    let mut config = proof_config();
    config.concurrency = 1;
    let state = state_with_rate_limit(
        Some((config, Some(snapshot_of(ring.commitment.clone())))),
        100,
    );
    let permit = state
        .proof
        .as_ref()
        .expect("proof state")
        .permits
        .clone()
        .try_acquire_owned()
        .expect("verification slot");
    let app = turn::routes(state);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(permit);
    });

    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 0)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn saturated_verification_times_out_with_retry_after() {
    let ring = test_ring();
    let mut config = proof_config();
    config.concurrency = 1;
    let state = state_with_rate_limit(
        Some((config, Some(snapshot_of(ring.commitment.clone())))),
        100,
    );
    let _permit = state
        .proof
        .as_ref()
        .expect("proof state")
        .permits
        .clone()
        .try_acquire_owned()
        .expect("verification slot");
    let app = turn::routes(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/turn/issue-with-proof")
                .header("content-type", "application/json")
                .body(Body::from(fresh_body(&ring, 0)))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers().get("retry-after").expect("header"), "1");
}

#[tokio::test]
async fn a_full_waiter_queue_rejects_another_request() {
    let ring = test_ring();
    let mut config = proof_config();
    config.concurrency = 1;
    let state = state_with_rate_limit(
        Some((config, Some(snapshot_of(ring.commitment.clone())))),
        100,
    );
    let proof_state = state.proof.as_ref().expect("proof state");
    let _permit = proof_state
        .permits
        .clone()
        .try_acquire_owned()
        .expect("verification slot");
    let _waiters: Vec<_> = (0..turn::http::state::MAX_PERMIT_WAITERS)
        .map(|_| {
            proof_state
                .waiters
                .clone()
                .try_acquire_owned()
                .expect("waiter slot")
        })
        .collect();
    let app = turn::routes(state);

    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(fresh_body(&ring, 0)),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn an_unknown_root_is_rejected_before_waiting_for_a_slot() {
    let ring = test_ring();
    let mut config = proof_config();
    config.concurrency = 1;
    let state = state_with_rate_limit(
        Some((config, Some(snapshot_of(ring.commitment.clone())))),
        100,
    );
    let _permit = state
        .proof
        .as_ref()
        .expect("proof state")
        .permits
        .clone()
        .try_acquire_owned()
        .expect("verification slot");
    let app = turn::routes(state);
    let timestamp = now_unix() + PROOF_MAX_SKEW_SECS;
    let proof = prove(&ring, 0, &context(), timestamp);

    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some(body_with_revision(&proof, 0, REVISION + 1, timestamp)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn proof_routes_absent_when_disabled() {
    let app = app(None);
    let (status, _) = post_json(
        &app,
        "/api/v1/turn/issue-with-proof",
        Some("{}".to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn wrong_method_returns_the_json_not_found() {
    let app = app(Some((proof_config(), None)));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method(axum::http::Method::GET)
                .uri("/api/v1/turn/issue-with-proof")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).expect("json"),
        serde_json::json!({ "error": "Not found" })
    );
}
