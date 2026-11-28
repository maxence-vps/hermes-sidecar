//! End-to-end tests of the model catalog routes, against a fake Hermes.

use axum::{
    Json, Router,
    body::Body,
    extract::RawQuery,
    http::{HeaderMap, Request, StatusCode, header},
    routing::get,
};
use hermes_sidecar::{AppState, Config, db, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tower::ServiceExt;

const KEY: &str = "test-key";
const ROUTES: [&str; 2] = ["/models", "/models/options"];

fn check_key(headers: &HeaderMap) {
    assert_eq!(
        headers[header::AUTHORIZATION],
        format!("Bearer {KEY}"),
        "the sidecar forwards the key to Hermes"
    );
}

fn models() -> Value {
    json!({
        "object": "list",
        "data": [{"id": "hermes-agent", "object": "model", "owned_by": "hermes"}],
    })
}

fn providers() -> Value {
    json!([
        {"slug": "openrouter", "models": ["anthropic/claude-opus-5-5"]},
        {"slug": "anthropic", "models": ["claude-opus-5-5"]},
    ])
}

/// Serves `hermes` on a random port. Returns its base URL.
async fn serve(hermes: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, hermes).await.unwrap() });
    format!("http://{addr}")
}

/// A fake Hermes that echoes the query of `/api/model/options`.
async fn fake_hermes() -> String {
    let hermes = Router::new()
        .route(
            "/v1/models",
            get(|headers: HeaderMap| async move {
                check_key(&headers);
                Json(models())
            }),
        )
        .route(
            "/api/model/options",
            get(|headers: HeaderMap, RawQuery(query): RawQuery| async move {
                check_key(&headers);
                Json(json!({"providers": providers(), "query": query}))
            }),
        );
    serve(hermes).await
}

/// A fake Hermes that answers `status` and `body` to everything.
async fn fixed_hermes(status: StatusCode, body: &'static str) -> String {
    let hermes = Router::new().fallback(move || async move { (status, body) });
    serve(hermes).await
}

async fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

/// A router over a fresh database, calling Hermes at `hermes_url`. Keep the `TempDir` alive for
/// the test's duration.
async fn app(hermes_url: &str) -> (Router, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sidecar.db");
    let config = Config::from_lookup(|name| match name {
        "API_SERVER_KEY" => Some(KEY.to_owned()),
        "HERMES_URL" => Some(hermes_url.to_owned()),
        "DATABASE_PATH" => Some(path.to_str().unwrap().to_owned()),
        "API_DOCS_ENABLED" => Some("true".to_owned()),
        _ => None,
    })
    .unwrap();
    let pool = db::open(&config.database_path).await.unwrap();
    (router(AppState::new(config, pool)), dir)
}

/// Sends a `GET` with `key` as bearer token, if any. Returns the status and the JSON body.
async fn send(app: &Router, path: &str, key: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::get(path);
    if let Some(key) = key {
        request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    let request = request.body(Body::empty()).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn model_routes_require_authentication() {
    let (app, _dir) = app(&fake_hermes().await).await;
    for path in ROUTES {
        for key in [None, Some("wrong-key")] {
            let (status, body) = send(&app, path, key).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} {key:?}");
            assert_eq!(body["error"]["code"], "unauthorized");
        }
    }
}

#[tokio::test]
async fn lists_models() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let (status, body) = send(&app, "/models", Some(KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, models());
}

#[tokio::test]
async fn lists_model_options_with_forwarded_query() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let cases = [
        ("", Value::Null),
        (
            "?include_unconfigured=false",
            json!("include_unconfigured=false"),
        ),
        (
            "?include_unconfigured=true",
            json!("include_unconfigured=true"),
        ),
        // Only `include_unconfigured` reaches Hermes.
        (
            "?other=x&include_unconfigured=true",
            json!("include_unconfigured=true"),
        ),
        ("?other=x", Value::Null),
    ];
    for (query, forwarded) in cases {
        let (status, body) = send(&app, &format!("/models/options{query}"), Some(KEY)).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(
            body,
            json!({"providers": providers(), "query": forwarded}),
            "{query}"
        );
    }
}

#[tokio::test]
async fn rejects_an_invalid_include_unconfigured() {
    let (app, _dir) = app(&fake_hermes().await).await;
    for value in ["maybe", "1", ""] {
        let path = format!("/models/options?include_unconfigured={value}");
        let (status, body) = send(&app, &path, Some(KEY)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }
}

#[tokio::test]
async fn model_routes_return_502_when_hermes_is_unreachable() {
    let (app, _dir) = app(&dead_url().await).await;
    for path in ROUTES {
        let (status, body) = send(&app, path, Some(KEY)).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
        assert_eq!(body["error"]["code"], "hermes_unavailable");
    }
}

#[tokio::test]
async fn model_routes_return_502_when_hermes_fails() {
    let failures = [
        (StatusCode::INTERNAL_SERVER_ERROR, "{}"),
        (StatusCode::UNAUTHORIZED, "{}"),
        (StatusCode::NOT_FOUND, "{}"),
        (StatusCode::OK, "not json"),
        (StatusCode::OK, ""),
    ];
    for (hermes_status, hermes_body) in failures {
        let (app, _dir) = app(&fixed_hermes(hermes_status, hermes_body).await).await;
        for path in ROUTES {
            let (status, body) = send(&app, path, Some(KEY)).await;
            assert_eq!(
                status,
                StatusCode::BAD_GATEWAY,
                "{path} {hermes_status} {hermes_body:?}"
            );
            assert_eq!(body["error"]["code"], "hermes_unavailable");
        }
    }
}

#[tokio::test]
async fn openapi_spec_describes_the_model_routes() {
    let (app, _dir) = app(&dead_url().await).await;
    let (status, spec) = send(&app, "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);

    for path in ROUTES {
        let operation = &spec["paths"][path]["get"];
        assert_eq!(operation["tags"], json!(["models"]), "{path}");
        assert_eq!(operation["security"], json!([{ "bearer": [] }]));
    }
    let params = spec["paths"]["/models/options"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["include_unconfigured"]);
    let tags = spec["tags"].as_array().unwrap();
    assert!(tags.iter().any(|tag| tag["name"] == "models"));
}
