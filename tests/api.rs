//! End-to-end tests of the router, with a fake Hermes served on a local port.

use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode, header},
    routing::get,
};
use hermes_sidecar::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

const KEY: &str = "test-key";

fn config(hermes_url: &str) -> Config {
    Config::from_lookup(|name| match name {
        "API_SERVER_KEY" => Some(KEY.to_owned()),
        "HERMES_URL" => Some(hermes_url.to_owned()),
        _ => None,
    })
    .unwrap()
}

/// Starts a fake Hermes that answers `GET /health` and returns its base URL.
async fn fake_hermes() -> String {
    let app = Router::new().route("/health", get(|| async { Json(json!({ "status": "ok" })) }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A URL where nothing listens: bind a port, then free it.
async fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

async fn send(app: Router, path: &str, auth: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::get(path);
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn health_is_public() {
    let app = router(AppState::new(config(&dead_url().await)));
    let (status, body) = send(app, "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn status_requires_the_shared_key() {
    let app = router(AppState::new(config(&fake_hermes().await)));

    let (status, body) = send(app.clone(), "/status", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    let (status, _) = send(app, "/status", Some("Bearer wrong-key")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn status_reports_a_reachable_hermes() {
    let app = router(AppState::new(config(&fake_hermes().await)));
    let (status, body) = send(app, "/status", Some(&format!("Bearer {KEY}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sidecar"]["status"], "ok");
    assert_eq!(body["hermes"]["reachable"], true);
    assert_eq!(body["hermes"]["health"]["status"], "ok");
}

#[tokio::test]
async fn status_is_a_bad_gateway_when_hermes_is_down() {
    let app = router(AppState::new(config(&dead_url().await)));
    let (status, body) = send(app, "/status", Some(&format!("Bearer {KEY}"))).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "hermes_unavailable");
}
