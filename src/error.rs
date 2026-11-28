//! Errors returned to API clients as JSON.

use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{hermes::HermesError, media::MediaError, repository_url::InvalidRepositoryUrl};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error(transparent)]
    Hermes(#[from] HermesError),
    #[error("the media route is not configured (HERMES_MEDIA_DIR is unset)")]
    MediaNotConfigured,
    #[error("the files route is not configured (HERMES_FILES_DIR is unset)")]
    FilesNotConfigured,
    #[error("no file found at this path")]
    FileNotFound,
    #[error("the file path is invalid")]
    InvalidFilePath,
    #[error("the file cannot be read: {0}")]
    FileUnreadable(String),
    #[error("file exceeds maximum allowed size")]
    FileTooLarge,
    #[error("the digest must be a SHA-256 in hex (64 characters)")]
    InvalidDigest,
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error("invalid request body: {}", .0.body_text())]
    InvalidBody(#[from] JsonRejection),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("no project with this id")]
    ProjectNotFound,
    #[error("a project with this name already exists")]
    ProjectNameTaken,
    #[error("{0}")]
    InvalidName(&'static str),
    #[error("no repository with this id in this project")]
    RepositoryNotFound,
    #[error("this repository is already attached to the project")]
    RepositoryAlreadyAttached,
    #[error(transparent)]
    InvalidRepositoryUrl(#[from] InvalidRepositoryUrl),
    #[error("the conversation id must be 1 to 255 printable ASCII characters, without spaces or /")]
    InvalidConversationId,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("Hermes has no session with this id")]
    SessionNotFound,
    #[error("a turn is already running in this conversation")]
    TurnAlreadyRunning,
    #[error("no stream active for this conversation")]
    StreamNotActive,
    #[error("the stream relay stopped before Hermes answered")]
    RelayFailed,
}

/// Body of every error response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code, e.g. `unauthorized` or `hermes_unavailable`.
    #[schema(example = "unauthorized")]
    pub code: String,
    /// Human-readable message.
    #[schema(example = "missing or invalid API key")]
    pub message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AppError::Hermes(_) => (StatusCode::BAD_GATEWAY, "hermes_unavailable"),
            AppError::MediaNotConfigured => {
                (StatusCode::SERVICE_UNAVAILABLE, "media_not_configured")
            }
            AppError::FilesNotConfigured => {
                (StatusCode::SERVICE_UNAVAILABLE, "files_not_configured")
            }
            AppError::FileNotFound => (StatusCode::NOT_FOUND, "file_not_found"),
            AppError::InvalidFilePath => (StatusCode::BAD_REQUEST, "invalid_file_path"),
            AppError::FileUnreadable(_) => (StatusCode::INTERNAL_SERVER_ERROR, "file_unreadable"),
            AppError::FileTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "file_too_large"),
            AppError::InvalidDigest => (StatusCode::BAD_REQUEST, "invalid_digest"),
            AppError::Media(MediaError::NotFound) => (StatusCode::NOT_FOUND, "media_not_found"),
            AppError::Media(MediaError::Io(_)) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "media_unreadable")
            }
            AppError::InvalidBody(rejection) => (rejection.status(), "invalid_body"),
            AppError::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, "database_error"),
            AppError::ProjectNotFound => (StatusCode::NOT_FOUND, "project_not_found"),
            AppError::ProjectNameTaken => (StatusCode::CONFLICT, "project_name_taken"),
            AppError::InvalidName(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_name"),
            AppError::RepositoryNotFound => (StatusCode::NOT_FOUND, "repository_not_found"),
            AppError::RepositoryAlreadyAttached => {
                (StatusCode::CONFLICT, "repository_already_attached")
            }
            AppError::InvalidRepositoryUrl(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid_repository_url")
            }
            AppError::InvalidConversationId => {
                (StatusCode::UNPROCESSABLE_ENTITY, "invalid_conversation_id")
            }
            AppError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            AppError::SessionNotFound => (StatusCode::NOT_FOUND, "session_not_found"),
            AppError::TurnAlreadyRunning => (StatusCode::CONFLICT, "turn_already_running"),
            AppError::StreamNotActive => (StatusCode::NOT_FOUND, "stream_not_active"),
            AppError::RelayFailed => (StatusCode::INTERNAL_SERVER_ERROR, "relay_failed"),
        };
        if status.is_server_error() {
            tracing::warn!(error = %self, "request failed");
        }
        let body = Json(ErrorBody {
            error: ErrorDetail {
                code: code.to_owned(),
                message: self.to_string(),
            },
        });
        let mut response = (status, body).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

/// `Json<T>` whose rejection (wrong content type, malformed JSON, missing field) is answered in
/// the same JSON shape as every other error, with the code `invalid_body`.
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state).await?;
        Ok(Self(value))
    }
}
