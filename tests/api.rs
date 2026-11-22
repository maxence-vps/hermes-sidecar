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
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::net::TcpListener;
use tower::ServiceExt;

const KEY: &str = "test-key";

fn config(hermes_url: &str) -> Config {
    config_with_docs(hermes_url, None)
}

fn config_with_docs(hermes_url: &str, docs: Option<&str>) -> Config {
    config_with(hermes_url, docs, None)
}

fn config_with(hermes_url: &str, docs: Option<&str>, media_dir: Option<&Path>) -> Config {
    Config::from_lookup(|name| match name {
        "API_SERVER_KEY" => Some(KEY.to_owned()),
        "HERMES_URL" => Some(hermes_url.to_owned()),
        "API_DOCS_ENABLED" => docs.map(str::to_owned),
        "HERMES_MEDIA_DIR" => media_dir.map(|dir| dir.to_str().unwrap().to_owned()),
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
    let media_op = &spec["paths"]["/media/{sha256}"]["get"];
    assert_eq!(media_op["security"], json!([{ "bearer": [] }]));
    let image = &media_op["responses"]["200"]["content"]["image/png"]["schema"];
    assert_eq!(image["$ref"], "#/components/schemas/ImageBytes");
    assert_eq!(
        spec["components"]["schemas"]["ImageBytes"]["format"],
        "binary"
    );
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

// --- GET /media/{sha256} ---

/// Bytes of a tiny file standing in for an image; the route never decodes it.
const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\nnot really a png";

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes `bytes` as `<root>/<folder>/<name>`, the layout of Hermes `native-inputs`.
fn store(root: &Path, folder: &str, name: &str, bytes: &[u8]) {
    let dir = root.join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn media_app(root: &Path) -> Router {
    router(AppState::new(config_with(
        "http://127.0.0.1:9",
        None,
        Some(root),
    )))
}

async fn get_media(app: Router, digest: &str, extra: &[(&str, &str)]) -> axum::response::Response {
    let mut request = Request::get(format!("/media/{digest}"))
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    app.oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn error_code(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["error"]["code"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn media_serves_a_stored_image() {
    let root = tempfile::tempdir().unwrap();
    let digest = digest(IMAGE);
    store(root.path(), &digest, "api_0123.png", IMAGE);

    // An uppercase digest is accepted too.
    let response = get_media(media_app(root.path()), &digest.to_uppercase(), &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers[header::ETAG], format!("\"{digest}\""));
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], IMAGE);
}

#[tokio::test]
async fn media_answers_304_for_a_known_etag() {
    let root = tempfile::tempdir().unwrap();
    let digest = digest(IMAGE);
    store(root.path(), &digest, "api_0123.jpg", IMAGE);
    let app = media_app(root.path());

    let etag = format!("W/\"other\", \"{digest}\"");
    let response = get_media(app.clone(), &digest, &[("if-none-match", &etag)]).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()[header::ETAG], format!("\"{digest}\""));
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    // A 304 is only for an image that is still there.
    std::fs::remove_dir_all(root.path().join(&digest)).unwrap();
    let response = get_media(app, &digest, &[("if-none-match", &etag)]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn media_requires_the_shared_key() {
    let root = tempfile::tempdir().unwrap();
    let request = Request::get(format!("/media/{}", digest(IMAGE)))
        .body(Body::empty())
        .unwrap();
    let response = media_app(root.path()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn media_rejects_an_invalid_digest() {
    let root = tempfile::tempdir().unwrap();
    for digest in ["abc", "..", &"z".repeat(64)] {
        let response = get_media(media_app(root.path()), digest, &[]).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{digest}");
        assert_eq!(error_code(response).await, "invalid_digest");
    }
}

#[tokio::test]
async fn media_is_404_for_an_unknown_digest() {
    let root = tempfile::tempdir().unwrap();
    let response = get_media(media_app(root.path()), &digest(IMAGE), &[]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(response).await, "media_not_found");
}

#[tokio::test]
async fn media_is_404_when_the_bytes_do_not_match_the_digest() {
    let root = tempfile::tempdir().unwrap();
    let digest = digest(IMAGE);
    store(root.path(), &digest, "api_0123.png", b"changed on disk");
    let response = get_media(media_app(root.path()), &digest, &[]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn media_only_serves_raster_images() {
    let root = tempfile::tempdir().unwrap();
    let svg = b"<svg onload=alert(1)/>";
    store(root.path(), &digest(svg), "doc.svg", svg);
    let pdf = b"%PDF-1.7";
    store(root.path(), &digest(pdf), "doc.pdf", pdf);
    for bytes in [&svg[..], &pdf[..]] {
        let response = get_media(media_app(root.path()), &digest(bytes), &[]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn media_ignores_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let digest = digest(IMAGE);
    std::fs::write(outside.path().join("secret.png"), IMAGE).unwrap();

    // A link in place of the file.
    std::fs::create_dir(root.path().join(&digest)).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.png"),
        root.path().join(&digest).join("api_0123.png"),
    )
    .unwrap();
    let response = get_media(media_app(root.path()), &digest, &[]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // A link in place of the folder.
    std::fs::remove_dir_all(root.path().join(&digest)).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join(&digest)).unwrap();
    let response = get_media(media_app(root.path()), &digest, &[]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn media_is_503_when_not_configured() {
    let app = router(AppState::new(config(&dead_url().await)));
    let response = get_media(app, &digest(IMAGE), &[]).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(response).await, "media_not_configured");
}
