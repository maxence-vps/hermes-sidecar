//! Reads the images Hermes keeps under its content-addressed `native-inputs` store.
//!
//! Hermes stores each image a client sends as `<native-inputs>/<sha256>/<name>`, where the folder
//! name is the SHA-256 of the file's bytes, and only keeps the text line
//! `[Image attached at: <path>]` in the message. Its API server has no route that returns the file,
//! so the sidecar reads it from the same volume, mounted read-only.

use std::{
    io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

/// Largest file served, same cap as the Hermes dashboard `GET /api/media`.
pub const MAX_BYTES: u64 = 25 * 1024 * 1024;

/// Image types served, by lowercase extension. Hermes only stores PNG, JPEG, GIF and WebP for
/// images; SVG is left out on purpose since it can carry scripts.
const CONTENT_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// An image read from the store, whose bytes match its digest.
#[derive(Debug)]
pub struct Image {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("no image stored for this digest")]
    NotFound,
    #[error("cannot read the media store: {0}")]
    Io(#[from] io::Error),
}

/// Returns the digest in lowercase if it is a SHA-256 in hex (64 characters), else `None`.
pub fn parse_digest(value: &str) -> Option<String> {
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

/// Content type of `path` if its extension is an allowed image type.
fn content_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    CONTENT_TYPES
        .iter()
        .find(|(known, _)| *known == ext)
        .map(|(_, content_type)| *content_type)
}

/// Finds the image stored for `digest` (already validated by [`parse_digest`]).
///
/// Only a real folder directly under `root` and real files in it count: symlinks are ignored, so
/// a link planted in the store cannot point the sidecar at another file. Hermes writes one file
/// per digest; if there are several, the first image by name wins.
pub async fn find(root: &Path, digest: &str) -> Result<(PathBuf, &'static str), MediaError> {
    let dir = root.join(digest);
    match tokio::fs::symlink_metadata(&dir).await {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(MediaError::NotFound),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(MediaError::NotFound);
        }
        Err(error) => return Err(error.into()),
    }

    let mut candidates = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let Some(content_type) = content_type(&path) else {
            continue;
        };
        // `file_type` does not follow symlinks.
        if entry.file_type().await?.is_file() {
            candidates.push((path, content_type));
        }
    }
    candidates.sort();
    candidates.into_iter().next().ok_or(MediaError::NotFound)
}

/// Reads the image stored for `digest` and checks that its bytes still hash to it.
///
/// A file over [`MAX_BYTES`] or whose bytes changed is reported as missing: the digest names
/// content, and that content is not available.
pub async fn read(root: &Path, digest: &str) -> Result<Image, MediaError> {
    let (path, content_type) = find(root, digest).await?;
    if tokio::fs::metadata(&path).await?.len() > MAX_BYTES {
        return Err(MediaError::NotFound);
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        // Removed between listing and reading, e.g. by the Hermes cleanup.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(MediaError::NotFound);
        }
        Err(error) => return Err(error.into()),
    };
    if hex(&Sha256::digest(&bytes)) != digest {
        tracing::warn!(%digest, path = %path.display(), "stored image does not match its digest");
        return Err(MediaError::NotFound);
    }
    Ok(Image {
        bytes,
        content_type,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_sha256_digest() {
        let digest = "A".repeat(64);
        assert_eq!(parse_digest(&digest), Some("a".repeat(64)));
        assert_eq!(parse_digest(&"a".repeat(63)), None);
        assert_eq!(parse_digest(&"g".repeat(64)), None);
        assert_eq!(parse_digest("../etc"), None);
    }

    #[test]
    fn only_allows_raster_images() {
        assert_eq!(content_type(Path::new("a.JPG")), Some("image/jpeg"));
        assert_eq!(content_type(Path::new("a.webp")), Some("image/webp"));
        assert_eq!(content_type(Path::new("a.svg")), None);
        assert_eq!(content_type(Path::new("a.pdf")), None);
        assert_eq!(content_type(Path::new("jpg")), None);
    }

    #[test]
    fn hashes_in_lowercase_hex() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
