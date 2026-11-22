//! End-to-end tests of the Projects routes, each against its own temporary database.

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use hermes_sidecar::{AppState, Config, db, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const KEY: &str = "test-key";

/// A router over a fresh database. Keep the `TempDir` alive for the test's duration.
async fn app() -> (Router, TempDir) {
    app_with_docs(false).await
}

async fn app_with_docs(docs: bool) -> (Router, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sidecar.db");
    let config = Config::from_lookup(|name| match name {
        "API_SERVER_KEY" => Some(KEY.to_owned()),
        "HERMES_URL" => Some("http://127.0.0.1:9".to_owned()),
        "DATABASE_PATH" => Some(path.to_str().unwrap().to_owned()),
        "API_DOCS_ENABLED" => Some(docs.to_string()),
        _ => None,
    })
    .unwrap();
    let pool = db::open(&config.database_path).await.unwrap();
    (router(AppState::new(config, pool)), dir)
}

/// Sends an authenticated request, with `body` as JSON when given. Returns the status and the
/// body parsed as JSON (`Null` when empty).
async fn call(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn create(app: &Router, name: &str) -> Value {
    let (status, project) = call(
        app,
        Method::POST,
        "/projects",
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    project
}

fn id(project: &Value) -> &str {
    project["id"].as_str().unwrap()
}

#[tokio::test]
async fn creates_and_reads_a_project() {
    let (app, _dir) = app().await;
    let project = create(&app, "  Hermes  ").await;
    assert_eq!(project["name"], "Hermes");
    assert_eq!(project["repositories"], json!([]));
    assert_eq!(project["conversation_ids"], json!([]));
    uuid::Uuid::parse_str(id(&project)).unwrap();

    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/projects/{}", id(&project)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, project);

    // The id is a UUID, accepted in any case.
    let upper = format!("/projects/{}", id(&project).to_uppercase());
    let (status, body) = call(&app, Method::GET, &upper, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, project);
}

#[tokio::test]
async fn lists_projects_by_name_ignoring_case() {
    let (app, _dir) = app().await;
    let (status, body) = call(&app, Method::GET, "/projects", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));

    for name in ["infra", "Hermes", "Écrits", "api"] {
        create(&app, name).await;
    }
    let (status, body) = call(&app, Method::GET, "/projects", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["api", "Hermes", "infra", "Écrits"]);
    assert_eq!(body[0]["repositories"], json!([]));
    assert_eq!(body[0]["conversation_ids"], json!([]));
}

#[tokio::test]
async fn rejects_an_invalid_name() {
    let (app, _dir) = app().await;
    for name in ["", "   ", &"a".repeat(101)] {
        let (status, body) = call(
            &app,
            Method::POST,
            "/projects",
            Some(json!({ "name": name })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name:?}");
        assert_eq!(body["error"]["code"], "invalid_name");
    }
    // 100 characters, not bytes.
    create(&app, &"é".repeat(100)).await;
}

#[tokio::test]
async fn rejects_a_malformed_body_as_json() {
    let (app, _dir) = app().await;
    let (status, body) = call(&app, Method::POST, "/projects", Some(json!({}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_body");

    let request = Request::post("/projects")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "invalid_body");
}

#[tokio::test]
async fn names_are_unique_ignoring_case() {
    let (app, _dir) = app().await;
    create(&app, "Équipe").await;
    for name in ["Équipe", "éQUIPE", " équipe "] {
        let (status, body) = call(
            &app,
            Method::POST,
            "/projects",
            Some(json!({ "name": name })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{name:?}");
        assert_eq!(body["error"]["code"], "project_name_taken");
    }
}

#[tokio::test]
async fn renames_a_project() {
    let (app, _dir) = app().await;
    let hermes = create(&app, "Hermes").await;
    create(&app, "Infra").await;
    let path = format!("/projects/{}", id(&hermes));

    let (status, body) = call(&app, Method::PATCH, &path, Some(json!({ "name": "Agent" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Agent");
    assert_eq!(body["id"], hermes["id"]);

    // Its own name with another case.
    let (status, body) = call(&app, Method::PATCH, &path, Some(json!({ "name": "AGENT" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "AGENT");

    // Another project's name.
    let (status, body) = call(&app, Method::PATCH, &path, Some(json!({ "name": "infra" }))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "project_name_taken");

    let (status, body) = call(&app, Method::PATCH, &path, Some(json!({ "name": " " }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_name");

    let (_, body) = call(&app, Method::GET, &path, None).await;
    assert_eq!(body["name"], "AGENT");
    // The old name is free again.
    create(&app, "Hermes").await;
}

#[tokio::test]
async fn deletes_a_project() {
    let (app, _dir) = app().await;
    let project = create(&app, "Hermes").await;
    let path = format!("/projects/{}", id(&project));

    let (status, body) = call(&app, Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);

    let (status, body) = call(&app, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "project_not_found");
    let (_, body) = call(&app, Method::GET, "/projects", None).await;
    assert_eq!(body, json!([]));
    // The name is free again.
    create(&app, "hermes").await;
}

#[tokio::test]
async fn unknown_projects_are_404() {
    let (app, _dir) = app().await;
    let unknown = format!("/projects/{}", uuid::Uuid::new_v4());
    for path in [unknown.as_str(), "/projects/not-a-uuid"] {
        for (method, body) in [
            (Method::GET, None),
            (Method::PATCH, Some(json!({ "name": "x" }))),
            (Method::DELETE, None),
        ] {
            let (status, body) = call(&app, method.clone(), path, body).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(body["error"]["code"], "project_not_found");
        }
    }
}

#[tokio::test]
async fn project_routes_require_the_shared_key() {
    let (app, _dir) = app().await;
    let id = uuid::Uuid::new_v4();
    for (method, path) in [
        (Method::GET, "/projects".to_owned()),
        (Method::POST, "/projects".to_owned()),
        (Method::GET, format!("/projects/{id}")),
        (Method::PATCH, format!("/projects/{id}")),
        (Method::DELETE, format!("/projects/{id}")),
    ] {
        let request = Request::builder()
            .method(method.clone())
            .uri(&path)
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
async fn openapi_spec_documents_the_project_routes() {
    let (app, _dir) = app_with_docs(true).await;
    let request = Request::get("/openapi.json").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let spec: Value = serde_json::from_slice(&bytes).unwrap();

    let expected = [
        ("/projects", "get", &["200", "401"][..]),
        ("/projects", "post", &["201", "400", "401", "409", "422"]),
        ("/projects/{id}", "get", &["200", "401", "404"]),
        (
            "/projects/{id}",
            "patch",
            &["200", "400", "401", "404", "409", "422"],
        ),
        ("/projects/{id}", "delete", &["204", "401", "404"]),
    ];
    for (path, method, codes) in expected {
        let op = &spec["paths"][path][method];
        assert_eq!(op["security"], json!([{ "bearer": [] }]), "{method} {path}");
        assert_eq!(op["tags"], json!(["projects"]), "{method} {path}");
        for code in codes {
            assert!(op["responses"][code].is_object(), "{method} {path} {code}");
        }
    }
    let project = &spec["components"]["schemas"]["Project"]["properties"];
    for field in ["id", "name", "repositories", "conversation_ids"] {
        assert!(project[field].is_object(), "{field}");
    }
}
