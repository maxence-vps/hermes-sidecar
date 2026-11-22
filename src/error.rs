//! Errors returned to API clients as JSON.

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{hermes::HermesError, media::MediaError};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error(transparent)]
    Hermes(#[from] HermesError),
    #[error("the media route is not configured (HERMES_MEDIA_DIR is unset)")]
    MediaNotConfigured,
    #[error("the digest must be a SHA-256 in hex (64 characters)")]
    InvalidDigest,
    #[error(transparent)]
    Media(#[from] MediaError),
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
            AppError::InvalidDigest => (StatusCode::BAD_REQUEST, "invalid_digest"),
            AppError::Media(MediaError::NotFound) => (StatusCode::NOT_FOUND, "media_not_found"),
            AppError::Media(MediaError::Io(_)) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "media_unreadable")
            }
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
