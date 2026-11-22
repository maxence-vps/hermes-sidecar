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
    config_with_docs(hermes_url, None)
}

fn config_with_docs(hermes_url: &str, docs: Option<&str>) -> Config {
    Config::from_lookup(|name| match name {
        "API_SERVER_KEY" => Some(KEY.to_owned()),
        "HERMES_URL" => Some(hermes_url.to_owned()),
        "API_DOCS_ENABLED" => docs.map(str::to_owned),
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

async fn send_raw(app: Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn send(app: Router, path: &str, auth: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::get(path);
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    let (status, body) = send_raw(app, request.body(Body::empty()).unwrap()).await;
    (status, serde_json::from_str(&body).unwrap())
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

#[tokio::test]
async fn docs_are_not_served_by_default() {
    let app = router(AppState::new(config(&dead_url().await)));
    for path in ["/openapi.json", "/docs"] {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let (status, _) = send_raw(app.clone(), request).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn openapi_spec_describes_the_routes_when_enabled() {
    let app = router(AppState::new(config_with_docs(
        &dead_url().await,
        Some("true"),
    )));
    let (status, spec) = send(app, "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);

    assert!(spec["paths"]["/health"]["get"].is_object());
    let status_op = &spec["paths"]["/status"]["get"];
    assert_eq!(status_op["security"], json!([{ "bearer": [] }]));
    for code in ["200", "401", "502"] {
        assert!(status_op["responses"][code].is_object(), "response {code}");
    }
    assert_eq!(
        spec["components"]["securitySchemes"]["bearer"]["scheme"],
        "bearer"
    );
    // The docs routes themselves are not part of the API description.
    assert!(spec["paths"]["/docs"].is_null());
    // No proxy prefix: no servers entry, so clients use the current origin.
    assert!(spec["servers"].is_null());
}

#[tokio::test]
async fn openapi_spec_follows_the_proxy_prefix() {
    let app = router(AppState::new(config_with_docs(
        &dead_url().await,
        Some("true"),
    )));
    let request = Request::get("/openapi.json")
        .header("x-forwarded-prefix", "/sidecar")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send_raw(app, request).await;
    assert_eq!(status, StatusCode::OK);
    let spec: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(spec["servers"], json!([{ "url": "/sidecar" }]));
}

#[tokio::test]
async fn scalar_docs_page_embeds_the_spec_when_enabled() {
    let app = router(AppState::new(config_with_docs(
        &dead_url().await,
        Some("true"),
    )));
    let request = Request::get("/docs").body(Body::empty()).unwrap();
    let (status, html) = send_raw(app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("<title>hermes-sidecar API</title>"));
    assert!(html.contains("@scalar/api-reference"));
    assert!(html.contains("\"/status\""));
}
