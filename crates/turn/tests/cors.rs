//! Browser reachability of `/api/v1/turn/issue-with-proof`: the preflight and
//! the allow-origin header a web product needs to read the minted credentials.
//! `/api/v1/turn/issue` is deliberately not browser-callable.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;
use turn::config::{Config, ProofConfig};
use turn::credentials::Algorithm;
use turn::AppState;

const PRODUCT: &str = "dim2.paseo";

fn app(proof: bool) -> axum::Router {
    let key = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
    turn::routes(AppState::new(Config {
        bind_addr: "127.0.0.1:0".parse().expect("valid addr"),
        turn_secret: b"cors-turn-secret".to_vec(),
        algorithm: Algorithm::Sha1,
        ttl_secs: 1800,
        realm: "example.org".to_string(),
        ice_servers: vec!["turn:turn.example.org:3478?transport=udp".to_string()],
        jwt_verifier: jwt_verify::Verifier::from_verifying_key(key.verifying_key()),
        rate_limit: 100,
        rate_window: Duration::from_secs(60),
        proof: proof.then(|| ProofConfig {
            rpc_url: "ws://unused.invalid".to_string(),
            genesis: [7u8; 32],
            contexts: [(
                PRODUCT.to_string(),
                turn::proof::context::product_context(PRODUCT, 0),
            )]
            .into_iter()
            .collect(),
            concurrency: 1,
        }),
    }))
}

async fn send(app: &axum::Router, request: Request<Body>) -> axum::http::Response<Body> {
    app.clone().oneshot(request).await.expect("response")
}

#[tokio::test]
async fn preflight_is_answered_on_the_proof_route() {
    let app = app(true);

    let response = send(
        &app,
        Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/turn/issue-with-proof")
            .header("origin", "https://dim2.example")
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization,content-type",
            )
            .body(Body::empty())
            .expect("request"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let headers = response.headers();
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .expect("allow-origin"),
        "*"
    );
    assert_eq!(
        headers
            .get("access-control-allow-methods")
            .expect("allow-methods"),
        "POST,OPTIONS"
    );
    assert_eq!(
        headers
            .get("access-control-allow-headers")
            .expect("allow-headers"),
        "authorization,content-type"
    );
}

#[tokio::test]
async fn responses_carry_allow_origin_so_a_browser_can_read_them() {
    let app = app(true);

    // A rejected body still has to be readable by the page that sent it.
    let response = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/turn/issue-with-proof")
            .header("origin", "https://dim2.example")
            .header("content-type", "application/json")
            .body(Body::from("{nope"))
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .expect("allow-origin"),
        "*"
    );
}

#[tokio::test]
async fn the_jwt_route_stays_out_of_the_browser() {
    let app = app(true);

    // No preflight: OPTIONS is a wrong method, so the JSON 404 answers.
    let response = send(
        &app,
        Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/turn/issue")
            .header("origin", "https://dim2.example")
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());

    let response = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/turn/issue")
            .header("origin", "https://dim2.example")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
}

#[tokio::test]
async fn the_layered_rejections_stay_readable_too() {
    let app = app(true);

    // Wrong method on the proof path: the JSON 404 comes from the method
    // router's fallback, which the CORS layer covers.
    let response = send(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/v1/turn/issue-with-proof")
            .header("origin", "https://dim2.example")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .expect("allow-origin"),
        "*"
    );

    // A body past the 8 KB cap is rejected under the CORS layer, so the page
    // can read that too.
    let response = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/turn/issue-with-proof")
            .header("origin", "https://dim2.example")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"productId":"{PRODUCT}","proof":"0x{}"}}"#,
                "aa".repeat(8 * 1024)
            )))
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .expect("allow-origin"),
        "*"
    );
}

#[tokio::test]
async fn the_preflight_does_not_answer_for_a_route_that_is_not_mounted() {
    let response = send(
        &app(false),
        Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/turn/issue-with-proof")
            .header("origin", "https://dim2.example")
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .expect("request"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
