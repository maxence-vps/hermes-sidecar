//! Runtime configuration, read from the environment.

use std::net::SocketAddr;

/// Default address the sidecar listens on.
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

/// Settings the sidecar needs to start.
#[derive(Clone)]
pub struct Config {
    /// Bearer key shared with Hermes (`API_SERVER_KEY`). Every protected route expects it.
    pub api_key: String,
    /// Base URL of the Hermes API server on the internal network, without a trailing slash.
    pub hermes_url: String,
    /// Address to listen on.
    pub bind_addr: SocketAddr,
}

// Keep the key out of logs and panic messages.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("api_key", &"<redacted>")
            .field("hermes_url", &self.hermes_url)
            .field("bind_addr", &self.bind_addr)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("environment variable {0} is empty")]
    Empty(&'static str),
    #[error("HERMES_URL must start with http:// or https://")]
    InvalidHermesUrl,
    #[error("BIND_ADDR is not a valid socket address: {0}")]
    InvalidBindAddr(String),
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `lookup`, so tests do not touch the process environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let api_key = required(&lookup, "API_SERVER_KEY")?;

        let hermes_url = required(&lookup, "HERMES_URL")?;
        if !(hermes_url.starts_with("http://") || hermes_url.starts_with("https://")) {
            return Err(ConfigError::InvalidHermesUrl);
        }
        let hermes_url = hermes_url.trim_end_matches('/').to_owned();

        let bind = lookup("BIND_ADDR")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_owned());
        let bind_addr = bind
            .parse()
            .map_err(|_| ConfigError::InvalidBindAddr(bind))?;

        Ok(Self {
            api_key,
            hermes_url,
            bind_addr,
        })
    }
}

fn required(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, ConfigError> {
    let value = lookup(name).ok_or(ConfigError::Missing(name))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(ConfigError::Empty(name));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn reads_all_values_and_trims_the_trailing_slash() {
        let config = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642/"),
            ("BIND_ADDR", "127.0.0.1:9000"),
        ]))
        .unwrap();
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.hermes_url, "http://hermes:8642");
        assert_eq!(config.bind_addr, "127.0.0.1:9000".parse().unwrap());
    }

    #[test]
    fn defaults_the_bind_address() {
        let config = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
        ]))
        .unwrap();
        assert_eq!(config.bind_addr, DEFAULT_BIND_ADDR.parse().unwrap());
    }

    #[test]
    fn refuses_to_start_without_a_key() {
        let missing = Config::from_lookup(lookup(&[("HERMES_URL", "http://hermes:8642")]));
        assert_eq!(missing.unwrap_err(), ConfigError::Missing("API_SERVER_KEY"));

        let empty = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "  "),
            ("HERMES_URL", "http://hermes:8642"),
        ]));
        assert_eq!(empty.unwrap_err(), ConfigError::Empty("API_SERVER_KEY"));
    }

    #[test]
    fn rejects_a_hermes_url_without_scheme() {
        let result = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "hermes:8642"),
        ]));
        assert_eq!(result.unwrap_err(), ConfigError::InvalidHermesUrl);
    }

    #[test]
    fn debug_output_hides_the_key() {
        let config = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
        ]))
        .unwrap();
        assert!(!format!("{config:?}").contains("test-key"));
    }
}
