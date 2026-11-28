//! Client for the Hermes API server, reached over the internal Docker network in plain HTTP.

use std::{fmt::Write as _, time::Duration};

use serde::de::DeserializeOwned;

use crate::config::Config;

/// Agent requests can run for ~30 minutes, so only the connection gets a short timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for quick probes such as the health check.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum HermesError {
    #[error("Hermes is unreachable: {0}")]
    Unreachable(#[source] reqwest::Error),
    #[error("Hermes answered with HTTP {0}")]
    Status(u16),
    #[error("Hermes sent an invalid response: {0}")]
    InvalidResponse(#[source] reqwest::Error),
}

/// Calls Hermes with the shared bearer key.
#[derive(Clone)]
pub struct HermesClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl HermesClient {
    pub fn new(config: &Config) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("the HTTP client configuration is static and valid");
        Self {
            http,
            base_url: config.hermes_url.clone(),
            api_key: config.api_key.clone(),
        }
    }

    /// `GET /health`. Hermes answers it without checking the key, so it only proves reachability.
    pub async fn health(&self) -> Result<serde_json::Value, HermesError> {
        self.get_json("/health", Some(PROBE_TIMEOUT)).await
    }

    /// Authenticated `GET {base_url}{path}`, decoded as JSON.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        timeout: Option<Duration>,
    ) -> Result<T, HermesError> {
        let mut request = self
            .http
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let response = request.send().await.map_err(HermesError::Unreachable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(HermesError::Status(status.as_u16()));
        }
        response.json().await.map_err(HermesError::InvalidResponse)
    }

    /// Starts an agent turn: authenticated `POST /api/sessions/{session_id}/chat/stream` with
    /// `body` as is. Returns as soon as Hermes answers with a success status, so that its SSE body
    /// can be read as it comes. No overall timeout: a turn can run for ~30 minutes, and dropping
    /// the response makes Hermes interrupt the turn.
    pub async fn chat_stream(
        &self,
        session_id: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, HermesError> {
        let url = format!(
            "{}/api/sessions/{}/chat/stream",
            self.base_url,
            encode_path_segment(session_id)
        );
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(body)
            .send()
            .await
            .map_err(HermesError::Unreachable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(HermesError::Status(status.as_u16()));
        }
        Ok(response)
    }
}

/// Percent-encodes everything but the RFC 3986 unreserved characters, so that a client value
/// cannot add a path segment, a query or a fragment to a Hermes URL.
fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            // Writing to a String cannot fail.
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_path_segments() {
        assert_eq!(
            encode_path_segment("20260412_101500_ab12cd"),
            "20260412_101500_ab12cd"
        );
        assert_eq!(encode_path_segment("a-b.c~d"), "a-b.c~d");
        assert_eq!(encode_path_segment("a?b#c%d/e"), "a%3Fb%23c%25d%2Fe");
        assert_eq!(encode_path_segment("é"), "%C3%A9");
    }
}
