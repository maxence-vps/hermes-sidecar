//! Runtime configuration, read from the environment.

use std::{net::SocketAddr, path::PathBuf};

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
    /// Serves the OpenAPI spec (`/openapi.json`) and the Scalar docs (`/docs`) when true
    /// (`API_DOCS_ENABLED`, false by default).
    pub docs_enabled: bool,
    /// Hermes `native-inputs` folder, mounted read-only, that `GET /media/{sha256}` reads
    /// (`HERMES_MEDIA_DIR`). Unset turns the route off: it answers 503.
    pub media_dir: Option<PathBuf>,
    /// Root directory that `GET /files/{*path}` reads, mounted read-only
    /// (`HERMES_FILES_DIR`). Unset turns the route off: it answers 503.
    pub files_dir: Option<PathBuf>,
    /// The sidecar's SQLite file (`DATABASE_PATH`), created on first start. Absolute.
    pub database_path: PathBuf,
}

// Keep the key out of logs and panic messages.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("api_key", &"<redacted>")
            .field("hermes_url", &self.hermes_url)
            .field("bind_addr", &self.bind_addr)
            .field("docs_enabled", &self.docs_enabled)
            .field("media_dir", &self.media_dir)
            .field("files_dir", &self.files_dir)
            .field("database_path", &self.database_path)
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
    #[error("{0} must be a boolean (true/false, 1/0, yes/no, on/off), got {1:?}")]
    InvalidBool(&'static str, String),
    #[error("HERMES_MEDIA_DIR must be an absolute path, got {0:?}")]
    InvalidMediaDir(String),
    #[error("HERMES_FILES_DIR must be an absolute path, got {0:?}")]
    InvalidFilesDir(String),
    #[error("DATABASE_PATH must be an absolute path, got {0:?}")]
    InvalidDatabasePath(String),
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

        let docs_enabled = boolean(&lookup, "API_DOCS_ENABLED", false)?;

        let media_dir = match lookup("HERMES_MEDIA_DIR").map(|v| v.trim().to_owned()) {
            None => None,
            Some(value) if value.is_empty() => None,
            Some(value) if PathBuf::from(&value).is_absolute() => Some(PathBuf::from(value)),
            Some(value) => return Err(ConfigError::InvalidMediaDir(value)),
        };

        let files_dir = match lookup("HERMES_FILES_DIR").map(|v| v.trim().to_owned()) {
            None => None,
            Some(value) if value.is_empty() => None,
            Some(value) if PathBuf::from(&value).is_absolute() => Some(PathBuf::from(value)),
            Some(value) => return Err(ConfigError::InvalidFilesDir(value)),
        };

        let database_path = required(&lookup, "DATABASE_PATH")?;
        if !PathBuf::from(&database_path).is_absolute() {
            return Err(ConfigError::InvalidDatabasePath(database_path));
        }
        let database_path = PathBuf::from(database_path);

        Ok(Self {
            api_key,
            hermes_url,
            bind_addr,
            docs_enabled,
            media_dir,
            files_dir,
            database_path,
        })
    }
}

/// Reads an optional boolean. Unset or empty means `default`; any other unknown value is an error,
/// so a typo never silently turns a feature on or off.
fn boolean(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: bool,
) -> Result<bool, ConfigError> {
    let Some(value) = lookup(name) else {
        return Ok(default);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "" => Ok(default),
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidBool(name, value)),
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
            ("DATABASE_PATH", "/data/sidecar.db"),
        ]))
        .unwrap();
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.hermes_url, "http://hermes:8642");
        assert_eq!(config.bind_addr, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(config.database_path, PathBuf::from("/data/sidecar.db"));
    }

    #[test]
    fn defaults_the_bind_address() {
        let config = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
            ("DATABASE_PATH", "/data/sidecar.db"),
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
    fn docs_are_disabled_by_default() {
        let config = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
            ("DATABASE_PATH", "/data/sidecar.db"),
        ]))
        .unwrap();
        assert!(!config.docs_enabled);
    }

    #[test]
    fn parses_the_docs_flag() {
        for (value, expected) in [
            ("true", true),
            ("TRUE", true),
            ("1", true),
            ("yes", true),
            ("on", true),
            ("false", false),
            ("0", false),
            ("no", false),
            ("off", false),
            ("", false),
        ] {
            let config = Config::from_lookup(lookup(&[
                ("API_SERVER_KEY", "test-key"),
                ("HERMES_URL", "http://hermes:8642"),
                ("DATABASE_PATH", "/data/sidecar.db"),
                ("API_DOCS_ENABLED", value),
            ]))
            .unwrap();
            assert_eq!(config.docs_enabled, expected, "value {value:?}");
        }
    }

    #[test]
    fn rejects_an_invalid_docs_flag() {
        let result = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
            ("DATABASE_PATH", "/data/sidecar.db"),
            ("API_DOCS_ENABLED", "enabled"),
        ]));
        assert_eq!(
            result.unwrap_err(),
            ConfigError::InvalidBool("API_DOCS_ENABLED", "enabled".to_owned())
        );
    }

    #[test]
    fn reads_the_media_dir() {
        let base = [
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
            ("DATABASE_PATH", "/data/sidecar.db"),
        ];
        let unset = Config::from_lookup(lookup(&base)).unwrap();
        assert_eq!(unset.media_dir, None);

        let mut pairs = base.to_vec();
        pairs.push(("HERMES_MEDIA_DIR", "/media/native-inputs"));
        let set = Config::from_lookup(lookup(&pairs)).unwrap();
        assert_eq!(set.media_dir, Some(PathBuf::from("/media/native-inputs")));

        let mut pairs = base.to_vec();
        pairs.push(("HERMES_MEDIA_DIR", "native-inputs"));
        assert_eq!(
            Config::from_lookup(lookup(&pairs)).unwrap_err(),
            ConfigError::InvalidMediaDir("native-inputs".to_owned())
        );
    }

    #[test]
    fn reads_the_files_dir() {
        let base = [
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
            ("DATABASE_PATH", "/data/sidecar.db"),
        ];
        let unset = Config::from_lookup(lookup(&base)).unwrap();
        assert_eq!(unset.files_dir, None);

        let mut pairs = base.to_vec();
        pairs.push(("HERMES_FILES_DIR", "/opt/data/projects"));
        let set = Config::from_lookup(lookup(&pairs)).unwrap();
        assert_eq!(set.files_dir, Some(PathBuf::from("/opt/data/projects")));

        let mut pairs = base.to_vec();
        pairs.push(("HERMES_FILES_DIR", "projects"));
        assert_eq!(
            Config::from_lookup(lookup(&pairs)).unwrap_err(),
            ConfigError::InvalidFilesDir("projects".to_owned())
        );
    }

    #[test]
    fn requires_an_absolute_database_path() {
        let base = [
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
        ];
        assert_eq!(
            Config::from_lookup(lookup(&base)).unwrap_err(),
            ConfigError::Missing("DATABASE_PATH")
        );

        let mut pairs = base.to_vec();
        pairs.push(("DATABASE_PATH", " "));
        assert_eq!(
            Config::from_lookup(lookup(&pairs)).unwrap_err(),
            ConfigError::Empty("DATABASE_PATH")
        );

        let mut pairs = base.to_vec();
        pairs.push(("DATABASE_PATH", "sidecar.db"));
        assert_eq!(
            Config::from_lookup(lookup(&pairs)).unwrap_err(),
            ConfigError::InvalidDatabasePath("sidecar.db".to_owned())
        );
    }

    #[test]
    fn debug_output_hides_the_key() {
        let config = Config::from_lookup(lookup(&[
            ("API_SERVER_KEY", "test-key"),
            ("HERMES_URL", "http://hermes:8642"),
            ("DATABASE_PATH", "/data/sidecar.db"),
        ]))
        .unwrap();
        assert!(!format!("{config:?}").contains("test-key"));
    }
}
