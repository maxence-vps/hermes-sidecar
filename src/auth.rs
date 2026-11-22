//! Bearer authentication with the key shared with Hermes (`API_SERVER_KEY`).

use axum::{
    extract::{Request, State},
    http::{HeaderMap, header},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use crate::{error::AppError, state::AppState};

/// Rejects the request with 401 unless it carries `Authorization: Bearer <API_SERVER_KEY>`.
pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if is_authorized(request.headers(), &state.config.api_key) {
        Ok(next.run(request).await)
    } else {
        Err(AppError::Unauthorized)
    }
}

/// Compares the bearer token with `expected` in constant time.
///
/// Only the length can leak through timing, which is acceptable for a long random key.
fn is_authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer")
        && bool::from(token.trim().as_bytes().ct_eq(expected.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn accepts_the_shared_key() {
        assert!(is_authorized(&headers("Bearer test-key"), "test-key"));
        assert!(is_authorized(&headers("bearer test-key"), "test-key"));
    }

    #[test]
    fn rejects_anything_else() {
        assert!(!is_authorized(&HeaderMap::new(), "test-key"));
        assert!(!is_authorized(&headers("Bearer wrong-key"), "test-key"));
        assert!(!is_authorized(&headers("Bearer test-ke"), "test-key"));
        assert!(!is_authorized(&headers("Basic test-key"), "test-key"));
        assert!(!is_authorized(&headers("test-key"), "test-key"));
        assert!(!is_authorized(&headers("Bearer "), "test-key"));
    }
}
