//! Session lifecycle: list, create, read, rename, delete and fork Hermes sessions through the
//! sidecar, so that clients never call Hermes directly.
//!
//! The bodies are Hermes's own, passed through as is. The sidecar adds the side effects on its
//! database: a deleted session leaves its project, and a fork takes the place of the original in
//! its project. See the "Sessions" section of `README.md`.

use axum::{
    Json,
    body::Bytes,
    extract::{
        Path, Query, State,
        rejection::{BytesRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    error::{AppError, ErrorBody, JsonBody},
    hermes::{HermesError, HermesJson, encode_path_segment},
    projects::validate_conversation_id,
    state::AppState,
    stream::validate_session_id,
};

/// Tag of the session routes in the OpenAPI spec.
const TAG: &str = "sessions";

/// The session routes, to merge into the protected router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_sessions, create_session))
        .routes(routes!(get_session, update_session, delete_session))
        .routes(routes!(fork_session))
}

/// A Hermes JSON body, passed through as is.
#[derive(ToSchema)]
#[schema(value_type = Object)]
pub struct HermesBody(#[allow(dead_code)] Value);

/// Query of `GET /sessions`, forwarded to Hermes.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListSessionsParams {
    /// Most sessions to return.
    #[param(example = 50)]
    pub limit: Option<u32>,
    /// Sessions to skip, for the next page.
    #[param(example = 0)]
    pub offset: Option<u32>,
}

/// Body of `POST /sessions`. Send `{}` for the Hermes defaults.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(default)]
pub struct CreateSessionInput {
    /// Model of the session. Hermes picks its default when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "anthropic/claude-opus-5-5")]
    pub model: Option<String>,
    /// Provider of the model. Hermes picks its default when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "anthropic")]
    pub provider: Option<String>,
}

/// Body of `PATCH /sessions/{id}`.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct UpdateSessionInput {
    /// New title of the session.
    #[schema(example = "Release checklist")]
    pub title: String,
}

/// Lists Hermes sessions, a page at a time.
///
/// Forwards `limit` and `offset` to Hermes `GET /api/sessions` and returns its answer as is.
#[utoipa::path(
    get,
    path = "/sessions",
    tag = TAG,
    security(("bearer" = [])),
    params(ListSessionsParams),
    responses(
        (status = OK, description = "A page of sessions, as Hermes returns it", body = HermesBody),
        (status = BAD_REQUEST, description = "`limit` or `offset` is not a non-negative integer (`invalid_request`)", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn list_sessions(
    State(state): State<AppState>,
    params: Result<Query<ListSessionsParams>, QueryRejection>,
) -> Result<Response, AppError> {
    let Query(params) =
        params.map_err(|rejection| AppError::InvalidRequest(rejection.body_text()))?;
    let mut query = Vec::new();
    if let Some(limit) = params.limit {
        query.push(format!("limit={limit}"));
    }
    if let Some(offset) = params.offset {
        query.push(format!("offset={offset}"));
    }
    let path = if query.is_empty() {
        "/api/sessions".to_owned()
    } else {
        format!("/api/sessions?{}", query.join("&"))
    };
    let answer = state.hermes.get(&path).await.map_err(hermes_error)?;
    Ok(forward(answer))
}

/// Creates a Hermes session.
///
/// Forwards `model` and `provider` to Hermes `POST /api/sessions` and returns the new session.
#[utoipa::path(
    post,
    path = "/sessions",
    tag = TAG,
    security(("bearer" = [])),
    request_body = CreateSessionInput,
    responses(
        (status = OK, description = "The new session, with the status Hermes answered with (`200` or `201`)", body = HermesBody),
        (status = BAD_REQUEST, description = "The body is not JSON (`invalid_body`), or Hermes rejected it (`invalid_request`)", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "`model` or `provider` is not a string (`invalid_body`)", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn create_session(
    State(state): State<AppState>,
    JsonBody(input): JsonBody<CreateSessionInput>,
) -> Result<Response, AppError> {
    let body = serde_json::to_value(&input).expect("two optional strings serialize to JSON");
    let answer = state
        .hermes
        .post("/api/sessions", Some(&body))
        .await
        .map_err(hermes_error)?;
    Ok(forward(answer))
}

/// Returns the metadata of a Hermes session.
#[utoipa::path(
    get,
    path = "/sessions/{id}",
    tag = TAG,
    security(("bearer" = [])),
    params(("id" = String, Path, description = "Hermes session id", example = "20260412_101500_ab12cd")),
    responses(
        (status = OK, description = "The session, as Hermes returns it", body = HermesBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = NOT_FOUND, description = "Hermes has no session with this id (`session_not_found`)", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid session id (`invalid_conversation_id`)", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    validate_session_id(&id)?;
    let answer = state
        .hermes
        .get(&session_path(&id))
        .await
        .map_err(hermes_error)?;
    Ok(forward(answer))
}

/// Renames a Hermes session.
#[utoipa::path(
    patch,
    path = "/sessions/{id}",
    tag = TAG,
    security(("bearer" = [])),
    params(("id" = String, Path, description = "Hermes session id", example = "20260412_101500_ab12cd")),
    request_body = UpdateSessionInput,
    responses(
        (status = OK, description = "The updated session, as Hermes returns it", body = HermesBody),
        (status = BAD_REQUEST, description = "The body is not JSON (`invalid_body`), or Hermes rejected it (`invalid_request`)", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = NOT_FOUND, description = "Hermes has no session with this id (`session_not_found`)", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid session id (`invalid_conversation_id`), or no string `title` (`invalid_body`)", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn update_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(input): JsonBody<UpdateSessionInput>,
) -> Result<Response, AppError> {
    validate_session_id(&id)?;
    let body = serde_json::to_value(&input).expect("a string serializes to JSON");
    let answer = state
        .hermes
        .patch(&session_path(&id), &body)
        .await
        .map_err(hermes_error)?;
    Ok(forward(answer))
}

/// Deletes a Hermes session and takes it out of its project.
///
/// The session also leaves its project when Hermes answers `404`, so that a session deleted
/// elsewhere does not stay referenced.
#[utoipa::path(
    delete,
    path = "/sessions/{id}",
    tag = TAG,
    security(("bearer" = [])),
    params(("id" = String, Path, description = "Hermes session id", example = "20260412_101500_ab12cd")),
    responses(
        (status = OK, description = "Deleted. Hermes's answer as is: `200` with its body, or `204`", body = HermesBody),
        (status = NO_CONTENT, description = "Deleted, and Hermes sent no body"),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = NOT_FOUND, description = "Hermes has no session with this id (`session_not_found`)", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid session id (`invalid_conversation_id`)", body = ErrorBody),
        (status = INTERNAL_SERVER_ERROR, description = "Deleted in Hermes, but the project link could not be removed (`database_error`)", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    validate_session_id(&id)?;
    match state.hermes.delete(&session_path(&id)).await {
        Ok(answer) => {
            detach(&state.db, &id).await?;
            Ok(forward(answer))
        }
        Err(HermesError::Status(404)) => {
            detach(&state.db, &id).await?;
            Err(AppError::SessionNotFound)
        }
        Err(error) => Err(hermes_error(error)),
    }
}

/// Forks a Hermes session. The fork takes the place of the original in its project.
///
/// The body, if any, is forwarded to Hermes `POST /api/sessions/{id}/fork` as is. The new session
/// id is read from `session.id` or `id` in the answer; when the original was in a project, the
/// fork is put in that project and the original is taken out of it.
#[utoipa::path(
    post,
    path = "/sessions/{id}/fork",
    tag = TAG,
    security(("bearer" = [])),
    params(("id" = String, Path, description = "Hermes session id of the original", example = "20260412_101500_ab12cd")),
    request_body(content = HermesBody, description = "Optional JSON object, forwarded to Hermes as is"),
    responses(
        (status = OK, description = "The fork, as Hermes returns it, with the status Hermes answered with (`200` or `201`)", body = HermesBody),
        (status = BAD_REQUEST, description = "The body is not a JSON object (`invalid_request`), or Hermes rejected it", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = NOT_FOUND, description = "Hermes has no session with this id (`session_not_found`)", body = ErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "Invalid session id (`invalid_conversation_id`)", body = ErrorBody),
        (status = INTERNAL_SERVER_ERROR, description = "Forked in Hermes, but the project could not be updated (`database_error`)", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn fork_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, AppError> {
    validate_session_id(&id)?;
    let body = parse_optional_object(body)?;
    let answer = state
        .hermes
        .post(&format!("{}/fork", session_path(&id)), body.as_ref())
        .await
        .map_err(hermes_error)?;
    match answer.body.as_ref().and_then(fork_id) {
        Some(fork_id) => transfer(&state.db, &id, fork_id).await?,
        None => tracing::warn!(
            session_id = %id,
            "Hermes forked the session without a usable new id; its project is left unchanged"
        ),
    }
    Ok(forward(answer))
}

/// Hermes path of a session, with the id percent-encoded.
fn session_path(id: &str) -> String {
    format!("/api/sessions/{}", encode_path_segment(id))
}

/// Hermes answers 404 for an unknown session, and 400 or 422 for a body it does not accept.
fn hermes_error(error: HermesError) -> AppError {
    match error {
        HermesError::Status(404) => AppError::SessionNotFound,
        HermesError::Status(status @ (400 | 422)) => {
            AppError::InvalidRequest(format!("Hermes rejected the request (HTTP {status})"))
        }
        error => AppError::Hermes(error),
    }
}

/// Answers with the status and body Hermes answered with.
fn forward(answer: HermesJson) -> Response {
    let status = StatusCode::from_u16(answer.status).unwrap_or(StatusCode::OK);
    match answer.body {
        Some(body) => (status, Json(body)).into_response(),
        None => status.into_response(),
    }
}

/// An empty body is `None`; anything else must be a JSON object.
fn parse_optional_object(body: Result<Bytes, BytesRejection>) -> Result<Option<Value>, AppError> {
    let body = body.map_err(|rejection| AppError::InvalidRequest(rejection.body_text()))?;
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    match serde_json::from_slice(&body) {
        Ok(object @ Value::Object(_)) => Ok(Some(object)),
        Ok(_) => Err(AppError::InvalidRequest(
            "the body must be a JSON object".to_owned(),
        )),
        Err(error) => Err(AppError::InvalidRequest(format!(
            "the body is not JSON: {error}"
        ))),
    }
}

/// Id of the new session in a Hermes fork answer: `session.id`, or else `id`. Only an id the
/// sidecar could store as a conversation counts.
fn fork_id(fork: &Value) -> Option<&str> {
    [&fork["session"]["id"], &fork["id"]]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|id| validate_conversation_id(id).is_ok())
}

/// Takes a session out of its project, if it is in one.
async fn detach(db: &SqlitePool, session_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM project_conversations WHERE conversation_id = ?")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Moves the project membership of `original` to `fork`, in one statement: nothing happens when
/// `original` is in no project, and a link `fork` already had is replaced.
async fn transfer(db: &SqlitePool, original: &str, fork: &str) -> Result<(), AppError> {
    if original == fork {
        return Ok(());
    }
    sqlx::query(
        "UPDATE OR REPLACE project_conversations SET conversation_id = ? \
         WHERE conversation_id = ?",
    )
    .bind(fork)
    .bind(original)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_the_fork_id() {
        assert_eq!(
            fork_id(&json!({"session": {"id": "a"}, "id": "b"})),
            Some("a")
        );
        assert_eq!(fork_id(&json!({"id": "b"})), Some("b"));
        assert_eq!(
            fork_id(&json!({"session": {"id": "a b"}, "id": "b"})),
            Some("b")
        );
        assert_eq!(fork_id(&json!({"session": {"id": 3}})), None);
        assert_eq!(fork_id(&json!([])), None);
    }

    #[test]
    fn maps_hermes_errors() {
        assert!(matches!(
            hermes_error(HermesError::Status(404)),
            AppError::SessionNotFound
        ));
        assert!(matches!(
            hermes_error(HermesError::Status(422)),
            AppError::InvalidRequest(_)
        ));
        assert!(matches!(
            hermes_error(HermesError::Status(500)),
            AppError::Hermes(_)
        ));
    }
}
