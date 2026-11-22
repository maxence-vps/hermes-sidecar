//! HTTP routes. Add new protected endpoints to `protected`.

use axum::{Json, Router, extract::State, middleware, routing::get};
use serde_json::{Value, json};

use crate::{auth::require_api_key, error::AppError, state::AppState};

pub fn router(state: AppState) -> Router {
    let protected =
        Router::new()
            .route("/status", get(status))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_api_key,
            ));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state)
}

/// Liveness probe for Docker and the reverse proxy. Public, and does not call Hermes.
async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Authenticated check that the sidecar accepts the key and can reach Hermes.
async fn status(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let hermes = state.hermes.health().await?;
    Ok(Json(json!({
        "sidecar": { "status": "ok", "version": env!("CARGO_PKG_VERSION") },
        "hermes": { "reachable": true, "health": hermes },
    })))
}
