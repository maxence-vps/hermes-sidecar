//! HTTP routes and their OpenAPI description.
//!
//! Add a protected endpoint by writing a handler annotated with `#[utoipa::path]` and registering
//! it in `protected` with `routes!(...)`: it is then routed, authenticated and documented at once.

use axum::{Json, Router, extract::State, middleware};
use serde::Serialize;
use utoipa::{
    Modify, OpenApi, ToSchema,
    openapi::security::{Http, HttpAuthScheme, SecurityScheme},
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::require_api_key,
    docs,
    error::{AppError, ErrorBody},
    state::AppState,
};

/// Name of the bearer security scheme in the OpenAPI spec.
pub const BEARER_SCHEME: &str = "bearer";

#[derive(OpenApi)]
#[openapi(
    info(
        title = "hermes-sidecar",
        description = "Custom endpoints next to a self-hosted Hermes Agent instance. \
            Protected routes take the same bearer key as Hermes (`API_SERVER_KEY`)."
    ),
    modifiers(&BearerAuth),
    tags((name = "sidecar", description = "Sidecar endpoints"))
)]
struct ApiDoc;

/// Declares the bearer scheme that protected routes reference with `security(("bearer" = []))`.
struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let mut http = Http::new(HttpAuthScheme::Bearer);
        http.description = Some("Same key as Hermes (`API_SERVER_KEY`).".to_owned());
        openapi
            .components
            .get_or_insert_with(Default::default)
            .add_security_scheme(BEARER_SCHEME, SecurityScheme::Http(http));
    }
}

pub fn router(state: AppState) -> Router {
    let docs_enabled = state.config.docs_enabled;

    let protected =
        OpenApiRouter::new()
            .routes(routes!(status))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_api_key,
            ));

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health))
        .merge(protected)
        .with_state(state)
        .split_for_parts();

    if docs_enabled {
        router.merge(docs::router(api))
    } else {
        router
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    #[schema(example = "ok")]
    pub status: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StatusResponse {
    pub sidecar: SidecarStatus,
    pub hermes: HermesStatus,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SidecarStatus {
    #[schema(example = "ok")]
    pub status: String,
    /// Sidecar version.
    #[schema(example = "0.1.0")]
    pub version: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HermesStatus {
    /// Always true in a 200 response: an unreachable Hermes answers 502.
    pub reachable: bool,
    /// Body of Hermes `GET /health`, passed through as is.
    #[schema(value_type = Object)]
    pub health: serde_json::Value,
}

/// Liveness probe for Docker and the reverse proxy. Public, and does not call Hermes.
#[utoipa::path(
    get,
    path = "/health",
    tag = "sidecar",
    responses((status = OK, description = "The sidecar is up", body = HealthResponse))
)]
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
    })
}

/// Checks that the key is accepted and that Hermes is reachable.
#[utoipa::path(
    get,
    path = "/status",
    tag = "sidecar",
    security(("bearer" = [])),
    responses(
        (status = OK, description = "Key accepted and Hermes reachable", body = StatusResponse),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = BAD_GATEWAY, description = "Hermes is unreachable or failed", body = ErrorBody),
    )
)]
async fn status(State(state): State<AppState>) -> Result<Json<StatusResponse>, AppError> {
    let health = state.hermes.health().await?;
    Ok(Json(StatusResponse {
        sidecar: SidecarStatus {
            status: "ok".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        hermes: HermesStatus {
            reachable: true,
            health,
        },
    }))
}
