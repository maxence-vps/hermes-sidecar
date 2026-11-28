//! Serves files from the workspace / files root directory configured via `HERMES_FILES_DIR`.
//!
//! Clients can download files (such as markdown reports, logs, code or generated assets)
//! produced by the agent or existing in the workspace.
//!
//! Paths are validated strictly against path traversal: relative paths cannot escape the configured
//! root folder, symlinks pointing outside the root are rejected, and only regular files are served.

use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    error::{AppError, ErrorBody},
    state::AppState,
};

/// Tag of the files routes in the OpenAPI spec.
const TAG: &str = "files";

/// Maximum file size served: 100 MiB.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// The files routes, to merge into the protected router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_file_query))
        .routes(routes!(get_file))
}

/// Raw file bytes, documented as a binary string.
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct FileBytes(#[allow(dead_code)] Vec<u8>);

/// Query of `GET /files`, for downloading by query parameter.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FileQueryParams {
    /// Relative or absolute path of the file to download
    #[param(example = "dokploy-hermes-setup.md")]
    pub path: String,
}

/// Known MIME content types by lowercase extension.
const CONTENT_TYPES: &[(&str, &str)] = &[
    ("md", "text/markdown; charset=utf-8"),
    ("markdown", "text/markdown; charset=utf-8"),
    ("txt", "text/plain; charset=utf-8"),
    ("text", "text/plain; charset=utf-8"),
    ("log", "text/plain; charset=utf-8"),
    ("json", "application/json"),
    ("yaml", "text/yaml; charset=utf-8"),
    ("yml", "text/yaml; charset=utf-8"),
    ("toml", "text/plain; charset=utf-8"),
    ("csv", "text/csv; charset=utf-8"),
    ("pdf", "application/pdf"),
    ("html", "text/html; charset=utf-8"),
    ("htm", "text/html; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("js", "application/javascript; charset=utf-8"),
    ("mjs", "application/javascript; charset=utf-8"),
    ("ts", "application/typescript; charset=utf-8"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("zip", "application/zip"),
    ("tar", "application/x-tar"),
    ("gz", "application/gzip"),
    ("sh", "text/x-shellscript; charset=utf-8"),
    ("bash", "text/x-shellscript; charset=utf-8"),
    ("py", "text/x-python; charset=utf-8"),
    ("rs", "text/x-rust; charset=utf-8"),
    ("swift", "text/x-swift; charset=utf-8"),
];

/// Returns the MIME content type of `path` based on its extension, defaulting to
/// `application/octet-stream`.
pub fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if let Some(ext) = ext {
        for (known_ext, mime) in CONTENT_TYPES {
            if *known_ext == ext.as_str() {
                return mime;
            }
        }
    }
    "application/octet-stream"
}

/// Resolves `raw_path` relative to `root`, strictly guarding against path traversal.
pub async fn resolve_file_path(root: &Path, raw_path: &str) -> Result<PathBuf, AppError> {
    let mut trimmed = raw_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::InvalidFilePath);
    }

    // If the caller provided a path that starts with the root folder itself, strip it.
    let root_str = root.to_str().unwrap_or("").trim_start_matches('/');
    if !root_str.is_empty() && trimmed.starts_with(root_str) {
        trimmed = trimmed[root_str.len()..].trim_start_matches('/');
    }
    if trimmed.is_empty() {
        return Err(AppError::InvalidFilePath);
    }

    // Path traversal verification: reject any ParentDir (..), CurDir (.), or RootDir components.
    let rel_path = Path::new(trimmed);
    for component in rel_path.components() {
        match component {
            std::path::Component::Normal(_) => {}
            _ => return Err(AppError::InvalidFilePath),
        }
    }

    let candidate = root.join(rel_path);

    // Canonicalize both root and candidate to resolve symlinks and ensure confinement.
    let canonical_root = tokio::fs::canonicalize(root)
        .await
        .map_err(|_| AppError::FilesNotConfigured)?;

    let canonical_candidate = match tokio::fs::canonicalize(&candidate).await {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::FileNotFound);
        }
        Err(err) => return Err(AppError::FileUnreadable(err.to_string())),
    };

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(AppError::FileNotFound);
    }

    let meta = match tokio::fs::metadata(&canonical_candidate).await {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::FileNotFound);
        }
        Err(err) => return Err(AppError::FileUnreadable(err.to_string())),
    };

    if !meta.is_file() {
        return Err(AppError::FileNotFound);
    }

    if meta.len() > MAX_FILE_BYTES {
        return Err(AppError::FileTooLarge);
    }

    Ok(canonical_candidate)
}

/// Reads the resolved file and builds the HTTP response with appropriate headers.
pub async fn serve_file(root: &Path, raw_path: &str) -> Result<Response, AppError> {
    let file_path = resolve_file_path(root, raw_path).await?;
    let bytes = match tokio::fs::read(&file_path).await {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::FileNotFound);
        }
        Err(err) => return Err(AppError::FileUnreadable(err.to_string())),
    };

    let mime = content_type(&file_path);
    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");

    let safe_filename: String = filename
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );

    let disposition = format!("attachment; filename=\"{safe_filename}\"");
    if let Ok(val) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, val);
    }

    Ok((StatusCode::OK, headers, bytes).into_response())
}

/// Downloads a file specified in the URL path.
#[utoipa::path(
    get,
    path = "/files/{*path}",
    tag = TAG,
    security(("bearer" = [])),
    params(
        ("path" = String, Path, description = "Relative path of the file to download", example = "dokploy-hermes-setup.md")
    ),
    responses(
        (status = OK, description = "The requested file", content(
            (FileBytes = "application/octet-stream"),
        )),
        (status = BAD_REQUEST, description = "Invalid file path (`invalid_file_path`)", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key (`unauthorized`)", body = ErrorBody),
        (status = NOT_FOUND, description = "File not found (`file_not_found`)", body = ErrorBody),
        (status = PAYLOAD_TOO_LARGE, description = "File exceeds maximum size (`file_too_large`)", body = ErrorBody),
        (status = INTERNAL_SERVER_ERROR, description = "File cannot be read (`file_unreadable`)", body = ErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "`HERMES_FILES_DIR` is unset (`files_not_configured`)", body = ErrorBody),
    )
)]
async fn get_file(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
) -> Result<Response, AppError> {
    let root = state
        .config
        .files_dir
        .as_deref()
        .ok_or(AppError::FilesNotConfigured)?;
    serve_file(root, &path).await
}

/// Downloads a file specified via the `path` query parameter.
#[utoipa::path(
    get,
    path = "/files",
    tag = TAG,
    security(("bearer" = [])),
    params(FileQueryParams),
    responses(
        (status = OK, description = "The requested file", content(
            (FileBytes = "application/octet-stream"),
        )),
        (status = BAD_REQUEST, description = "Invalid file path (`invalid_file_path`)", body = ErrorBody),
        (status = UNAUTHORIZED, description = "Missing or invalid API key (`unauthorized`)", body = ErrorBody),
        (status = NOT_FOUND, description = "File not found (`file_not_found`)", body = ErrorBody),
        (status = PAYLOAD_TOO_LARGE, description = "File exceeds maximum size (`file_too_large`)", body = ErrorBody),
        (status = INTERNAL_SERVER_ERROR, description = "File cannot be read (`file_unreadable`)", body = ErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "`HERMES_FILES_DIR` is unset (`files_not_configured`)", body = ErrorBody),
    )
)]
async fn get_file_query(
    State(state): State<AppState>,
    Query(params): Query<FileQueryParams>,
) -> Result<Response, AppError> {
    let root = state
        .config
        .files_dir
        .as_deref()
        .ok_or(AppError::FilesNotConfigured)?;
    serve_file(root, &params.path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_content_types() {
        assert_eq!(
            content_type(Path::new("test.md")),
            "text/markdown; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("test.TXT")),
            "text/plain; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("test.json")), "application/json");
        assert_eq!(
            content_type(Path::new("test.yaml")),
            "text/yaml; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("test.unknown")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type(Path::new("no_ext")),
            "application/octet-stream"
        );
    }
}
