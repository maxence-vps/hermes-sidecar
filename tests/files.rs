//! End-to-end tests of the files route.

use std::path::Path;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use hermes_sidecar::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const KEY: &str = "test-key";

fn config_with_files(files_dir: Option<&Path>) -> Config {
    Config::from_lookup(|name| match name {
        "API_SERVER_KEY" => Some(KEY.to_owned()),
        "HERMES_URL" => Some("http://127.0.0.1:9".to_owned()),
        "API_DOCS_ENABLED" => Some("true".to_owned()),
        "HERMES_FILES_DIR" => files_dir.map(|dir| dir.to_str().unwrap().to_owned()),
        "DATABASE_PATH" => Some("/unused/sidecar.db".to_owned()),
        _ => None,
    })
    .unwrap()
}

fn app(files_dir: Option<&Path>) -> Router {
    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_lazy("sqlite::memory:")
        .unwrap();
    router(AppState::new(config_with_files(files_dir), db))
}

async fn get_file_request(
    app: Router,
    path_or_query: &str,
    auth: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::get(path_or_query);
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    app.oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn files_requires_authentication() {
    let root = tempfile::tempdir().unwrap();
    let app = app(Some(root.path()));

    let response = get_file_request(app.clone(), "/files/test.md", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "unauthorized");

    let response = get_file_request(app.clone(), "/files/test.md", Some("Bearer wrong-key")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = get_file_request(app, "/files?path=test.md", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn files_serves_a_file_by_path() {
    let root = tempfile::tempdir().unwrap();
    let file_content = b"# Hermes Setup\n\nInstructions...";
    std::fs::write(root.path().join("dokploy-hermes-setup.md"), file_content).unwrap();

    let app = app(Some(root.path()));
    let response = get_file_request(
        app,
        "/files/dokploy-hermes-setup.md",
        Some(&format!("Bearer {KEY}")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "text/markdown; charset=utf-8"
    );
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"dokploy-hermes-setup.md\""
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], file_content);
}

#[tokio::test]
async fn files_serves_a_file_in_a_nested_directory() {
    let root = tempfile::tempdir().unwrap();
    let nested_dir = root.path().join("sub").join("nested");
    std::fs::create_dir_all(&nested_dir).unwrap();
    let file_content = b"{\"status\": \"ok\"}";
    std::fs::write(nested_dir.join("data.json"), file_content).unwrap();

    let app = app(Some(root.path()));
    let response = get_file_request(
        app,
        "/files/sub/nested/data.json",
        Some(&format!("Bearer {KEY}")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"data.json\""
    );

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], file_content);
}

#[tokio::test]
async fn files_serves_a_file_via_query_parameter() {
    let root = tempfile::tempdir().unwrap();
    let file_content = b"log entries here";
    std::fs::write(root.path().join("output.log"), file_content).unwrap();

    let app = app(Some(root.path()));
    let response = get_file_request(
        app,
        "/files?path=output.log",
        Some(&format!("Bearer {KEY}")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"output.log\""
    );

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], file_content);
}

#[tokio::test]
async fn files_strips_root_prefix_if_full_path_is_given() {
    let root = tempfile::tempdir().unwrap();
    let file_content = b"hello";
    std::fs::write(root.path().join("test.txt"), file_content).unwrap();

    let root_str = root.path().to_str().unwrap().trim_start_matches('/');
    let app = app(Some(root.path()));

    let response = get_file_request(
        app,
        &format!("/files/{root_str}/test.txt"),
        Some(&format!("Bearer {KEY}")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], file_content);
}

#[tokio::test]
async fn files_rejects_path_traversal() {
    let root = tempfile::tempdir().unwrap();
    let app = app(Some(root.path()));

    for bad_path in ["/files/../secret", "/files/a/../../secret"] {
        let response =
            get_file_request(app.clone(), bad_path, Some(&format!("Bearer {KEY}"))).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad_path}");
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "invalid_file_path");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn files_rejects_symlinks_pointing_outside() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();

    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        root.path().join("link_to_secret.txt"),
    )
    .unwrap();

    let app = app(Some(root.path()));
    let response = get_file_request(
        app,
        "/files/link_to_secret.txt",
        Some(&format!("Bearer {KEY}")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "file_not_found");
}

#[tokio::test]
async fn files_returns_404_for_directory() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("some_dir")).unwrap();

    let app = app(Some(root.path()));
    let response = get_file_request(app, "/files/some_dir", Some(&format!("Bearer {KEY}"))).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "file_not_found");
}

#[tokio::test]
async fn files_returns_404_for_nonexistent_file() {
    let root = tempfile::tempdir().unwrap();
    let app = app(Some(root.path()));

    let response =
        get_file_request(app, "/files/missing.md", Some(&format!("Bearer {KEY}"))).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "file_not_found");
}

#[tokio::test]
async fn files_returns_503_when_not_configured() {
    let app = app(None);
    let response = get_file_request(app, "/files/test.md", Some(&format!("Bearer {KEY}"))).await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "files_not_configured");
}

#[tokio::test]
async fn openapi_spec_describes_the_files_routes() {
    let root = tempfile::tempdir().unwrap();
    let app = app(Some(root.path()));

    let response = get_file_request(app, "/openapi.json", None).await;
    assert_eq!(response.status(), StatusCode::OK);

    let spec = response_json(response).await;
    let files_op = &spec["paths"]["/files/{*path}"]["get"];
    assert_eq!(files_op["tags"], serde_json::json!(["files"]));
    assert_eq!(files_op["security"], serde_json::json!([{ "bearer": [] }]));

    let query_op = &spec["paths"]["/files"]["get"];
    assert_eq!(query_op["tags"], serde_json::json!(["files"]));
    assert_eq!(query_op["security"], serde_json::json!([{ "bearer": [] }]));
}
