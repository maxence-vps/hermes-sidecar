//! Model catalog: the model aliases and the provider options of Hermes, so that clients never call
//! Hermes directly.
//!
//! The bodies are Hermes's own, passed through as is and not cached, so a provider configured in
//! Hermes shows up at once. See the "Models" section of `README.md`.

use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
};
use serde::Deserialize;
use serde_json::Value;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    error::{AppError, ErrorBody},
    hermes::REQUEST_TIMEOUT,
    sessions::HermesBody,
    state::AppState,
};

/// Tag of the model routes in the OpenAPI spec.
const TAG: &str = "models";

/// The model routes, to merge into the protected router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_models))
        .routes(routes!(model_options))
}

/// Query of `GET /models/options`, forwarded to Hermes.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ModelOptionsParams {
    /// Whether to include the providers that are not configured. Hermes picks its default when
    /// absent.
    #[param(example = false)]
    pub include_unconfigured: Option<bool>,
}

/// Lists the model aliases Hermes serves.
///
/// Returns the answer of Hermes `GET /v1/models` as is.
#[utoipa::path(
    get,
    path = "/models",
    tag = TAG,
    security(("bearer" = [])),
    responses(
        (status = OK, description = "The model aliases, as Hermes returns them", body = HermesBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn list_models(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let models = state
        .hermes
        .get_json("/v1/models", Some(REQUEST_TIMEOUT))
        .await?;
    Ok(Json(models))
}

/// Lists the models of each provider.
///
/// Forwards `include_unconfigured` to Hermes `GET /api/model/options` and returns its answer as is.
#[utoipa::path(
    get,
    path = "/models/options",
    tag = TAG,
    security(("bearer" = [])),
    params(ModelOptionsParams),
    responses(
        (status = OK, description = "The models grouped by provider, as Hermes returns them", body = HermesBody),
        (status = BAD_REQUEST, description = "`include_unconfigured` is not `true` or `false` (`invalid_request`)", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed (`hermes_unavailable`)", body = ErrorBody),
    )
)]
async fn model_options(
    State(state): State<AppState>,
    params: Result<Query<ModelOptionsParams>, QueryRejection>,
) -> Result<Json<Value>, AppError> {
    let Query(params) =
        params.map_err(|rejection| AppError::InvalidRequest(rejection.body_text()))?;
    let path = match params.include_unconfigured {
        Some(include) => format!("/api/model/options?include_unconfigured={include}"),
        None => "/api/model/options".to_owned(),
    };
    let options = state.hermes.get_json(&path, Some(REQUEST_TIMEOUT)).await?;
    Ok(Json(options))
}
