//! End-to-end tests for the chat stream relay.

use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::Path,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::stream;
use hermes_sidecar::{AppState, Config, router};
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::{net::TcpListener, sync::Notify};
use tower::ServiceExt;

const KEY: &str = "test-key";

fn config(hermes_url: &str) -> Config {
    Config::from_lookup(|name| match name {
        "API_SERVER_KEY" => Some(KEY.to_owned()),
        "HERMES_URL" => Some(hermes_url.to_owned()),
        "DATABASE_PATH" => Some("/unused/sidecar.db".to_owned()),
        _ => None,
    })
    .unwrap()
}

fn app(config: Config) -> Router {
    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_lazy("sqlite::memory:")
        .unwrap();
    router(AppState::new(config, db))
}

async fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[tokio::test]
async fn stream_requires_authentication() {
    let app = app(config(&dead_url().await));

    let request = Request::post("/sessions/20261120_093000_a1b2c3/chat/stream")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message": "Hello"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stream_validates_session_id() {
    let app = app(config(&dead_url().await));
    let too_long = "x".repeat(256);

    for bad_id in ["a%2Fb", "a%20b", "%C3%A9", too_long.as_str()] {
        let request = Request::post(format!("/sessions/{bad_id}/chat/stream"))
            .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message": "Hello"}"#))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "invalid_conversation_id");
    }
}

#[tokio::test]
async fn stream_requires_a_message() {
    let app = app(config(&dead_url().await));

    for bad_body in ["{}", r#"{"message": null}"#, "not-json"] {
        let request = Request::post("/sessions/20261120_093000_a1b2c3/chat/stream")
            .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(bad_body))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "invalid_request");
    }
}

#[tokio::test]
async fn stream_returns_502_when_hermes_is_unreachable() {
    let app = app(config(&dead_url().await));

    let request = Request::post("/sessions/20261120_093000_a1b2c3/chat/stream")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message": "Hello"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "hermes_unavailable");
}

#[tokio::test]
async fn stream_relays_events_from_hermes() {
    let fake_hermes = Router::new().route(
        "/api/sessions/{session_id}/chat/stream",
        post(|Path(id): Path<String>| async move {
            assert_eq!(id, "20261120_093000_a1b2c3");
            let stream = stream::iter(vec![
                Ok::<_, Infallible>("event: run.started\ndata: {}\n\n"),
                Ok("event: assistant.delta\ndata: {\"delta\":\"Hello\"}\n\n"),
                Ok("event: done\ndata: {}\n\n"),
            ]);
            let body = Body::from_stream(stream);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(body)
                .unwrap()
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, fake_hermes).await.unwrap() });

    let app = app(config(&format!("http://{addr}")));

    let request = Request::post("/sessions/20261120_093000_a1b2c3/chat/stream")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message": "Hi"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("event: run.started\ndata: {}\n\n"));
    assert!(text.contains("event: assistant.delta\ndata: {\"delta\":\"Hello\"}\n\n"));
    assert!(text.contains("event: done\ndata: {}\n\n"));
}

#[tokio::test]
async fn stream_returns_404_when_session_not_found() {
    let fake_hermes = Router::new().route(
        "/api/sessions/{session_id}/chat/stream",
        post(|| async { StatusCode::NOT_FOUND.into_response() }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, fake_hermes).await.unwrap() });

    let app = app(config(&format!("http://{addr}")));

    let request = Request::post("/sessions/unknown_session/chat/stream")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message": "Hi"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "session_not_found");
}

#[tokio::test]
async fn stream_rejects_duplicate_active_turn() {
    let proceed = Arc::new(Notify::new());
    let proceed_clone = proceed.clone();

    let fake_hermes = Router::new().route(
        "/api/sessions/{session_id}/chat/stream",
        post(move || {
            let proceed = proceed_clone.clone();
            async move {
                let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, Infallible>>(2);
                tokio::spawn(async move {
                    let _ = tx
                        .send(Ok("event: run.started\ndata: {}\n\n".to_owned()))
                        .await;
                    proceed.notified().await;
                    let _ = tx.send(Ok("event: done\ndata: {}\n\n".to_owned())).await;
                });
                let stream = stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|item| (item, rx))
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, fake_hermes).await.unwrap() });

    let app = app(config(&format!("http://{addr}")));

    let req1 = Request::post("/sessions/test_turn/chat/stream")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message": "First"}"#))
        .unwrap();

    let res1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(res1.status(), StatusCode::OK);

    // Second request while turn 1 is still active
    let req2 = Request::post("/sessions/test_turn/chat/stream")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message": "Second"}"#))
        .unwrap();

    let res2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::CONFLICT);
    let body = res2.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "turn_already_running");

    // Release turn 1
    proceed.notify_one();
    let _ = res1.into_body().collect().await;
}

#[tokio::test]
async fn stream_decouples_client_disconnect() {
    let completed_on_hermes = Arc::new(AtomicBool::new(false));
    let completed_flag = completed_on_hermes.clone();

    let fake_hermes = Router::new().route(
        "/api/sessions/{session_id}/chat/stream",
        post(move || {
            let completed = completed_flag.clone();
            async move {
                let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, Infallible>>(4);
                tokio::spawn(async move {
                    let _ = tx
                        .send(Ok("event: run.started\ndata: {}\n\n".to_owned()))
                        .await;
                    // Simulate delay between events
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = tx
                        .send(Ok(
                            "event: assistant.delta\ndata: {\"delta\":\"Still going\"}\n\n"
                                .to_owned(),
                        ))
                        .await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = tx.send(Ok("event: done\ndata: {}\n\n".to_owned())).await;
                    completed.store(true, Ordering::SeqCst);
                });
                let stream = stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|item| (item, rx))
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, fake_hermes).await.unwrap() });

    let app = app(config(&format!("http://{addr}")));

    let request = Request::post("/sessions/decouple_session/chat/stream")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message": "Hello"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Client drops response stream immediately
    drop(response);

    // Wait and verify Hermes stream runs to completion in sidecar background task
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(completed_on_hermes.load(Ordering::SeqCst));
}
