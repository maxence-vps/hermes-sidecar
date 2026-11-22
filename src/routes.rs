//! HTTP routes and their OpenAPI description.
//!
//! Add a protected endpoint by writing a handler annotated with `#[utoipa::path]` and registering
//! it in `protected` with `routes!(...)`: it is then routed, authenticated and documented at once.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
};
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
    media, projects,
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
    tags(
        (name = "sidecar", description = "Sidecar endpoints"),
        (name = "projects", description = "Projects group Conversations (Hermes sessions) and git Repositories"),
    )
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

    let protected = OpenApiRouter::new()
        .routes(routes!(status))
        .routes(routes!(get_media))
        .merge(projects::router())
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

/// Raw image bytes, documented as a binary string.
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct ImageBytes(#[allow(dead_code)] Vec<u8>);

/// Images never change for a digest, so clients may keep them for a year.
const MEDIA_CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// Returns an image a client attached to a message.
///
/// Hermes keeps only the line `[Image attached at: <dir>/<sha256>/<name>]` in the message text;
/// pass the `<sha256>` folder name from that path. The sidecar checks that the file still hashes to
/// it before sending it.
#[utoipa::path(
    get,
    path = "/media/{sha256}",
    tag = "sidecar",
    security(("bearer" = [])),
    params(
        ("sha256" = String, Path, description = "SHA-256 of the image, in hex: the folder name in the `[Image attached at: …]` path",
            example = "418ff2aae3eb452752ef0e7d1e7c06049ac0bf43f7736f483509f4fe1b4eb0bf"),
        ("If-None-Match" = Option<String>, Header, description = "ETag from an earlier response, to get a 304"),
    ),
    responses(
        (status = OK, description = "The image. `ETag` is the quoted digest.", content(
            (ImageBytes = "image/png"),
            (ImageBytes = "image/jpeg"),
            (ImageBytes = "image/gif"),
            (ImageBytes = "image/webp"),
        )),
        (status = NOT_MODIFIED, description = "The client already holds this image"),
        (status = BAD_REQUEST, description = "The digest is not a SHA-256 in hex", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key", body = ErrorBody),
        (status = NOT_FOUND, description = "No image for this digest: never sent, deleted by Hermes, or changed on disk", body = ErrorBody),
        (status = INTERNAL_SERVER_ERROR, description = "The media folder cannot be read", body = ErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "`HERMES_MEDIA_DIR` is not set", body = ErrorBody),
    )
)]
async fn get_media(
    State(state): State<AppState>,
    Path(sha256): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let root = state
        .config
        .media_dir
        .as_deref()
        .ok_or(AppError::MediaNotConfigured)?;
    let digest = media::parse_digest(&sha256).ok_or(AppError::InvalidDigest)?;
    let etag = format!("\"{digest}\"");

    if if_none_match(&headers, &etag) {
        // Still answer 404 for a digest that is gone, so a client can tell.
        media::find(root, &digest).await?;
        return Ok(cached_headers(StatusCode::NOT_MODIFIED, &etag).into_response());
    }

    let image = media::read(root, &digest).await?;
    let (status, mut headers) = cached_headers(StatusCode::OK, &etag);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(image.content_type),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok((status, headers, image.bytes).into_response())
}

fn cached_headers(status: StatusCode, etag: &str) -> (StatusCode, HeaderMap) {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("a quoted hex digest is a valid header value"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(MEDIA_CACHE_CONTROL),
    );
    (status, headers)
}

/// True when `If-None-Match` lists `etag` (weak or strong) or `*`.
fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|tag| tag.trim().trim_start_matches("W/"))
        .any(|tag| tag == "*" || tag == etag)
}
