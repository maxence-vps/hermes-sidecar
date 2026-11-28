//! End-to-end tests of the session lifecycle routes, against a fake Hermes and a temporary
//! database.

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, RawQuery},
    http::{HeaderMap, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hermes_sidecar::{AppState, Config, db, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tower::ServiceExt;

const KEY: &str = "test-key";

/// Session ids the fake Hermes treats specially.
const MISSING: &str = "20260101_000000_missing";
const BROKEN: &str = "20260101_000000_broken";
const NO_FORK_ID: &str = "20260101_000000_noforkid";
const WITH_BODY: &str = "20260101_000000_withbody";

fn check_key(headers: &HeaderMap) {
    assert_eq!(
        headers[header::AUTHORIZATION],
        format!("Bearer {KEY}"),
        "the sidecar forwards the key to Hermes"
    );
}

/// Answers the special ids with their failure, or `None` to go on.
fn special(id: &str) -> Option<Response> {
    match id {
        MISSING => Some(StatusCode::NOT_FOUND.into_response()),
        BROKEN => Some(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        _ => None,
    }
}

fn parse(body: &Bytes) -> Value {
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).unwrap()
    }
}

/// A fake Hermes on a random port. Returns its base URL.
async fn fake_hermes() -> String {
    let hermes = Router::new()
        .route(
            "/api/sessions",
            get(|headers: HeaderMap, RawQuery(query): RawQuery| async move {
                check_key(&headers);
                Json(json!({
                    "sessions": [{"id": "20260412_101500_ab12cd"}],
                    "query": query,
                }))
            })
            .post(|headers: HeaderMap, body: Bytes| async move {
                check_key(&headers);
                let input = parse(&body);
                if input["model"] == "unknown" {
                    return StatusCode::UNPROCESSABLE_ENTITY.into_response();
                }
                let session = json!({"id": "20260412_101500_new", "input": input});
                (StatusCode::CREATED, Json(json!({ "session": session }))).into_response()
            }),
        )
        .route(
            "/api/sessions/{id}",
            get(|Path(id): Path<String>| async move {
                special(&id)
                    .unwrap_or_else(|| Json(json!({"id": id, "title": "T"})).into_response())
            })
            .patch(|Path(id): Path<String>, body: Bytes| async move {
                special(&id).unwrap_or_else(|| {
                    let input = parse(&body);
                    Json(json!({"id": id, "title": input["title"]})).into_response()
                })
            })
            .delete(|Path(id): Path<String>| async move {
                special(&id).unwrap_or_else(|| {
                    if id == WITH_BODY {
                        Json(json!({"deleted": true})).into_response()
                    } else {
                        StatusCode::NO_CONTENT.into_response()
                    }
                })
            }),
        )
        .route(
            "/api/sessions/{id}/fork",
            post(|Path(id): Path<String>, body: Bytes| async move {
                special(&id).unwrap_or_else(|| {
                    if id == NO_FORK_ID {
                        return Json(json!({"ok": true})).into_response();
                    }
                    Json(json!({
                        "session": {"id": format!("{id}_fork")},
                        "forked_from": id,
                        "body": parse(&body),
                    }))
                    .into_response()
                })
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, hermes).await.unwrap() });
    format!("http://{addr}")
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

/// Sends an authenticated request, with `body` as JSON when given. Returns the status and the
/// body parsed as JSON (`Null` when empty).
async fn call(app: &Router, method: Method, path: &str, body: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_owned())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, parse(&bytes))
}

/// Creates a project and puts `conversations` in it. Returns its id.
async fn project_with(app: &Router, name: &str, conversations: &[&str]) -> String {
    let body = json!({ "name": name }).to_string();
    let (status, project) = call(app, Method::POST, "/projects", Some(&body)).await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    let id = project["id"].as_str().unwrap().to_owned();
    for conversation in conversations {
        let path = format!("/projects/{id}/conversations/{conversation}");
        let (status, _) = call(app, Method::PUT, &path, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    id
}

/// The conversation ids of a project, sorted.
async fn conversations(app: &Router, project_id: &str) -> Vec<String> {
    let path = format!("/projects/{project_id}");
    let (status, project) = call(app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK, "{project}");
    let mut ids: Vec<String> = project["conversation_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn session_routes_require_authentication() {
    let (app, _dir) = app(&dead_url().await).await;
    let routes = [
        (Method::GET, "/sessions"),
        (Method::POST, "/sessions"),
        (Method::GET, "/sessions/s1"),
        (Method::PATCH, "/sessions/s1"),
        (Method::DELETE, "/sessions/s1"),
        (Method::POST, "/sessions/s1/fork"),
    ];
    for (method, path) in routes {
        let request = Request::builder()
            .method(method.clone())
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn lists_sessions_with_pagination() {
    let (app, _dir) = app(&fake_hermes().await).await;

    let (status, body) = call(&app, Method::GET, "/sessions?limit=20&offset=40", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"][0]["id"], "20260412_101500_ab12cd");
    assert_eq!(body["query"], "limit=20&offset=40");

    let (status, body) = call(&app, Method::GET, "/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["query"], Value::Null);

    // Only `limit` and `offset` reach Hermes.
    let (status, body) = call(&app, Method::GET, "/sessions?offset=5&other=x", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["query"], "offset=5");
}

#[tokio::test]
async fn rejects_invalid_pagination() {
    let (app, _dir) = app(&fake_hermes().await).await;
    for query in ["limit=-1", "offset=abc", "limit=1.5"] {
        let (status, body) = call(&app, Method::GET, &format!("/sessions?{query}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }
}

#[tokio::test]
async fn creates_a_session() {
    let (app, _dir) = app(&fake_hermes().await).await;

    let body = r#"{"model": "anthropic/claude-opus-5-5", "provider": "anthropic"}"#;
    let (status, created) = call(&app, Method::POST, "/sessions", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "Hermes's status is kept");
    assert_eq!(created["session"]["id"], "20260412_101500_new");
    assert_eq!(
        created["session"]["input"],
        json!({"model": "anthropic/claude-opus-5-5", "provider": "anthropic"})
    );

    // Absent fields are not sent to Hermes.
    let (status, created) = call(&app, Method::POST, "/sessions", Some("{}")).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["session"]["input"], json!({}));
}

#[tokio::test]
async fn create_validates_the_body() {
    let (app, _dir) = app(&fake_hermes().await).await;

    let (status, body) = call(&app, Method::POST, "/sessions", Some("not-json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_body");

    let (status, body) = call(&app, Method::POST, "/sessions", Some(r#"{"model": 3}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_body");

    // Hermes refuses the model.
    let (status, body) = call(
        &app,
        Method::POST,
        "/sessions",
        Some(r#"{"model": "unknown"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn gets_a_session() {
    let (app, _dir) = app(&fake_hermes().await).await;

    let path = "/sessions/20260412_101500_ab12cd";
    let (status, session) = call(&app, Method::GET, path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session,
        json!({"id": "20260412_101500_ab12cd", "title": "T"})
    );

    let path = format!("/sessions/{MISSING}");
    let (status, body) = call(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "session_not_found");

    let path = format!("/sessions/{BROKEN}");
    let (status, body) = call(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "hermes_unavailable");
}

#[tokio::test]
async fn validates_session_ids() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let too_long = "x".repeat(256);
    for bad_id in ["a%2Fb", "a%20b", "%C3%A9", "%2E%2E", too_long.as_str()] {
        let requests = [
            (Method::GET, format!("/sessions/{bad_id}"), None),
            (
                Method::PATCH,
                format!("/sessions/{bad_id}"),
                Some(r#"{"title": "x"}"#),
            ),
            (Method::DELETE, format!("/sessions/{bad_id}"), None),
            (Method::POST, format!("/sessions/{bad_id}/fork"), None),
        ];
        for (method, path, body) in requests {
            let (status, json) = call(&app, method.clone(), &path, body).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{method} {path}");
            assert_eq!(json["error"]["code"], "invalid_conversation_id");
        }
    }
}

#[tokio::test]
async fn renames_a_session() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let path = "/sessions/20260412_101500_ab12cd";

    let (status, session) = call(&app, Method::PATCH, path, Some(r#"{"title": "New"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session,
        json!({"id": "20260412_101500_ab12cd", "title": "New"})
    );

    let (status, body) = call(&app, Method::PATCH, path, Some("{}")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_body");

    let (status, body) = call(&app, Method::PATCH, path, Some("{")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_body");

    let missing = format!("/sessions/{MISSING}");
    let (status, body) = call(&app, Method::PATCH, &missing, Some(r#"{"title": "x"}"#)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "session_not_found");
}

#[tokio::test]
async fn deleting_a_session_takes_it_out_of_its_project() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let project = project_with(&app, "Hermes", &["s1", "s2", WITH_BODY, MISSING]).await;

    let (status, body) = call(&app, Method::DELETE, "/sessions/s1", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    assert_eq!(
        conversations(&app, &project).await,
        [MISSING, WITH_BODY, "s2"]
    );

    // Hermes's body is passed through.
    let path = format!("/sessions/{WITH_BODY}");
    let (status, body) = call(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"deleted": true}));
    assert_eq!(conversations(&app, &project).await, [MISSING, "s2"]);

    // A session Hermes no longer has leaves its project too.
    let path = format!("/sessions/{MISSING}");
    let (status, body) = call(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "session_not_found");
    assert_eq!(conversations(&app, &project).await, ["s2"]);
}

#[tokio::test]
async fn a_failed_delete_keeps_the_project_link() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let project = project_with(&app, "Hermes", &[BROKEN]).await;

    let path = format!("/sessions/{BROKEN}");
    let (status, body) = call(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "hermes_unavailable");
    assert_eq!(conversations(&app, &project).await, [BROKEN]);
}

#[tokio::test]
async fn a_fork_takes_the_place_of_the_original_in_its_project() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let project = project_with(&app, "Hermes", &["s1", "s2"]).await;

    let (status, fork) = call(&app, Method::POST, "/sessions/s1/fork", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fork["session"]["id"], "s1_fork");
    assert_eq!(fork["forked_from"], "s1");
    assert_eq!(fork["body"], Value::Null, "no body is sent to Hermes");
    assert_eq!(conversations(&app, &project).await, ["s1_fork", "s2"]);
}

#[tokio::test]
async fn a_fork_replaces_a_link_the_new_session_already_had() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let first = project_with(&app, "First", &["s1"]).await;
    let second = project_with(&app, "Second", &["s1_fork"]).await;

    let (status, _) = call(&app, Method::POST, "/sessions/s1/fork", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(conversations(&app, &first).await, ["s1_fork"]);
    assert!(conversations(&app, &second).await.is_empty());
}

#[tokio::test]
async fn forking_a_session_outside_projects_changes_no_project() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let project = project_with(&app, "Hermes", &["s2"]).await;

    let body = r#"{"up_to_message": 3}"#;
    let (status, fork) = call(&app, Method::POST, "/sessions/s1/fork", Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fork["body"],
        json!({"up_to_message": 3}),
        "the body is forwarded"
    );
    assert_eq!(conversations(&app, &project).await, ["s2"]);
}

#[tokio::test]
async fn a_fork_without_a_new_id_leaves_the_project_unchanged() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let project = project_with(&app, "Hermes", &[NO_FORK_ID]).await;

    let path = format!("/sessions/{NO_FORK_ID}/fork");
    let (status, fork) = call(&app, Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fork, json!({"ok": true}));
    assert_eq!(conversations(&app, &project).await, [NO_FORK_ID]);
}

#[tokio::test]
async fn fork_errors() {
    let (app, _dir) = app(&fake_hermes().await).await;
    let project = project_with(&app, "Hermes", &[MISSING]).await;

    let path = format!("/sessions/{MISSING}/fork");
    let (status, body) = call(&app, Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "session_not_found");
    assert_eq!(conversations(&app, &project).await, [MISSING]);

    for bad_body in ["not-json", "[1]", "3"] {
        let path = "/sessions/s1/fork";
        let (status, body) = call(&app, Method::POST, path, Some(bad_body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad_body}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }
}

#[tokio::test]
async fn session_routes_return_502_when_hermes_is_unreachable() {
    let (app, _dir) = app(&dead_url().await).await;
    let requests = [
        (Method::GET, "/sessions", None),
        (Method::POST, "/sessions", Some("{}")),
        (Method::GET, "/sessions/s1", None),
        (Method::PATCH, "/sessions/s1", Some(r#"{"title": "x"}"#)),
        (Method::DELETE, "/sessions/s1", None),
        (Method::POST, "/sessions/s1/fork", None),
    ];
    for (method, path, body) in requests {
        let (status, json) = call(&app, method.clone(), path, body).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{method} {path}");
        assert_eq!(json["error"]["code"], "hermes_unavailable");
    }
}

#[tokio::test]
async fn openapi_spec_describes_the_session_routes() {
    let (app, _dir) = app(&dead_url().await).await;
    let request = Request::get("/openapi.json").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let spec: Value = serde_json::from_slice(&bytes).unwrap();

    let operations = [
        ("/sessions", "get"),
        ("/sessions", "post"),
        ("/sessions/{id}", "get"),
        ("/sessions/{id}", "patch"),
        ("/sessions/{id}", "delete"),
        ("/sessions/{id}/fork", "post"),
    ];
    for (path, method) in operations {
        let operation = &spec["paths"][path][method];
        assert_eq!(operation["tags"], json!(["sessions"]), "{method} {path}");
        assert_eq!(operation["security"], json!([{ "bearer": [] }]));
    }
    let params = spec["paths"]["/sessions"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["limit", "offset"]);
}
