//! Errors returned to API clients as JSON.

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::hermes::HermesError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error(transparent)]
    Hermes(#[from] HermesError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AppError::Hermes(_) => (StatusCode::BAD_GATEWAY, "hermes_unavailable"),
        };
        if status.is_server_error() {
            tracing::warn!(error = %self, "request failed");
        }
        let body = Json(json!({ "error": { "code": code, "message": self.to_string() } }));
        let mut response = (status, body).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
