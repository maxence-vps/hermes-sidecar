//! OpenAPI spec (`/openapi.json`) and Scalar docs (`/docs`), mounted only when
//! `API_DOCS_ENABLED` is true. Both are public: they describe the routes, not any data.

use std::sync::Arc;

use axum::{Json, Router, extract::State, http::HeaderMap, response::Html, routing::get};
use utoipa::openapi::{OpenApi, Server};
use utoipa_scalar::Scalar;

pub fn router(api: OpenApi) -> Router {
    Router::new()
        .route("/openapi.json", get(spec))
        .route("/docs", get(docs))
        .with_state(Arc::new(api))
}

async fn spec(State(api): State<Arc<OpenApi>>, headers: HeaderMap) -> Json<OpenApi> {
    Json(for_request(&api, &headers))
}

async fn docs(State(api): State<Arc<OpenApi>>, headers: HeaderMap) -> Html<String> {
    Html(
        Scalar::new(for_request(&api, &headers))
            .title("hermes-sidecar API")
            .to_html(),
    )
}

/// Copies the spec and points its server at the path prefix the reverse proxy stripped, so
/// "Try it" calls `/sidecar/status` and not `/status` when served under `/sidecar`.
fn for_request(api: &OpenApi, headers: &HeaderMap) -> OpenApi {
    let mut api = api.clone();
    if let Some(prefix) = forwarded_prefix(headers) {
        api.servers = Some(vec![Server::new(prefix)]);
    }
    api
}

/// Reads `X-Forwarded-Prefix` (set by Traefik's strip-prefix middleware).
///
/// The value ends up inside the HTML page, so only a plain path is accepted.
fn forwarded_prefix(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("x-forwarded-prefix")?.to_str().ok()?;
    let prefix = value.trim().trim_end_matches('/');
    let is_plain_path = prefix.starts_with('/')
        && prefix.len() <= 128
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~'));
    is_plain_path.then(|| prefix.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(prefix: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-prefix", HeaderValue::from_str(prefix).unwrap());
        headers
    }

    #[test]
    fn reads_a_plain_prefix() {
        assert_eq!(
            forwarded_prefix(&headers("/sidecar")).as_deref(),
            Some("/sidecar")
        );
        assert_eq!(
            forwarded_prefix(&headers("/sidecar/")).as_deref(),
            Some("/sidecar")
        );
        assert_eq!(
            forwarded_prefix(&headers("/a/b-c_d.e")).as_deref(),
            Some("/a/b-c_d.e")
        );
    }

    #[test]
    fn ignores_missing_or_unsafe_prefixes() {
        assert_eq!(forwarded_prefix(&HeaderMap::new()), None);
        assert_eq!(forwarded_prefix(&headers("/")), None);
        assert_eq!(forwarded_prefix(&headers("sidecar")), None);
        assert_eq!(forwarded_prefix(&headers("/x</script>")), None);
        assert_eq!(forwarded_prefix(&headers("/x\"y")), None);
        assert_eq!(forwarded_prefix(&headers("https://evil.test/x")), None);
    }
}
