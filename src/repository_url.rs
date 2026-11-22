//! Normalises the remote URL of a git Repository attached to a Project.
//!
//! Accepted forms, all turned into `https://host/path`:
//!
//! - `https://host/owner/name`
//! - `git@host:owner/name` (scp-like syntax)
//! - `ssh://git@host/owner/name`
//!
//! The host is lowercased and a trailing `.git` or `/` is removed. The path keeps its case, since
//! GitLab and Gitea paths are case-sensitive.

/// Longest URL accepted, before normalisation.
const MAX_LEN: usize = 2048;

/// A normalised repository URL and the name derived from it.
#[derive(Debug, PartialEq, Eq)]
pub struct RepositoryUrl {
    /// `https://host/path`.
    pub url: String,
    /// The whole path after the host, e.g. `owner/name` or `org/team/api`.
    pub name: String,
}

/// Why a URL was rejected, shown to the client.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvalidRepositoryUrl {
    #[error(
        "the URL must be https://host/owner/name, git@host:owner/name or ssh://git@host/owner/name"
    )]
    UnsupportedForm,
    #[error("the URL is longer than 2048 characters")]
    TooLong,
    #[error("the URL has an invalid host")]
    InvalidHost,
    #[error("the URL must not carry credentials, a query or a fragment")]
    ExtraParts,
    #[error("the path must have at least two segments, like owner/name")]
    InvalidPath,
}

/// Normalises `input`, or says why it is not a supported git remote URL.
pub fn normalise(input: &str) -> Result<RepositoryUrl, InvalidRepositoryUrl> {
    let input = input.trim();
    if input.len() > MAX_LEN {
        return Err(InvalidRepositoryUrl::TooLong);
    }
    if input.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(InvalidRepositoryUrl::UnsupportedForm);
    }

    let (host, path) = if let Some(rest) = strip_prefix_ignore_case(input, "https://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.contains('@') {
            return Err(InvalidRepositoryUrl::ExtraParts);
        }
        (parse_host(authority, true)?, path)
    } else if let Some(rest) = strip_prefix_ignore_case(input, "ssh://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = match authority.split_once('@') {
            Some((user, host)) if valid_user(user) => host,
            Some(_) => return Err(InvalidRepositoryUrl::ExtraParts),
            None => authority,
        };
        // The SSH port means nothing for the https URL.
        let host = match host.rsplit_once(':') {
            Some((host, port)) if valid_port(port) => host,
            Some(_) => return Err(InvalidRepositoryUrl::InvalidHost),
            None => host,
        };
        (parse_host(host, false)?, path)
    } else if input.contains("://") {
        // http://, file://, git://...
        return Err(InvalidRepositoryUrl::UnsupportedForm);
    } else {
        // scp-like: user@host:path. Without a user, `host:path` could be a local path.
        let (user, rest) = input
            .split_once('@')
            .ok_or(InvalidRepositoryUrl::UnsupportedForm)?;
        let (host, path) = rest
            .split_once(':')
            .ok_or(InvalidRepositoryUrl::UnsupportedForm)?;
        if !valid_user(user) {
            return Err(InvalidRepositoryUrl::UnsupportedForm);
        }
        (
            parse_host(host, false)?,
            path.strip_prefix('/').unwrap_or(path),
        )
    };

    let name = parse_path(path)?;
    Ok(RepositoryUrl {
        url: format!("https://{host}/{name}"),
        name,
    })
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &value[prefix.len()..])
}

fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
}

/// Lowercases a DNS host name, with a port when `allow_port` (kept in the https URL).
fn parse_host(authority: &str, allow_port: bool) -> Result<String, InvalidRepositoryUrl> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if allow_port && valid_port(port) => (host, Some(port)),
        Some(_) => return Err(InvalidRepositoryUrl::InvalidHost),
        None => (authority, None),
    };
    let valid = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if !valid {
        return Err(InvalidRepositoryUrl::InvalidHost);
    }
    let host = host.to_ascii_lowercase();
    Ok(match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// Removes a trailing `/` and `.git`, then checks the segments. Returns the path.
fn parse_path(path: &str) -> Result<String, InvalidRepositoryUrl> {
    if path.contains(['?', '#']) {
        return Err(InvalidRepositoryUrl::ExtraParts);
    }
    let path = path.trim_end_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    let valid = segments.len() >= 2
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && *segment != "."
                && *segment != ".."
                && !segment.contains(['\\', '%', ':', '@'])
        });
    if !valid {
        return Err(InvalidRepositoryUrl::InvalidPath);
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(input: &str) -> (String, String) {
        let normalised = normalise(input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
        (normalised.url, normalised.name)
    }

    fn url(input: &str) -> String {
        ok(input).0
    }

    #[test]
    fn accepts_https() {
        assert_eq!(
            ok("https://github.com/owner/hermes-sidecar"),
            (
                "https://github.com/owner/hermes-sidecar".to_owned(),
                "owner/hermes-sidecar".to_owned()
            )
        );
        assert_eq!(
            url("HTTPS://GitHub.COM/Owner/Name"),
            "https://github.com/Owner/Name"
        );
        assert_eq!(
            url("https://git.example.test:8443/o/n"),
            "https://git.example.test:8443/o/n"
        );
    }

    #[test]
    fn accepts_scp_like_ssh() {
        assert_eq!(
            url("git@github.com:owner/hermes-sidecar.git"),
            "https://github.com/owner/hermes-sidecar"
        );
        assert_eq!(
            url("git@GitLab.com:/org/team/api"),
            "https://gitlab.com/org/team/api"
        );
    }

    #[test]
    fn accepts_ssh_urls() {
        assert_eq!(
            url("ssh://git@github.com/owner/name.git"),
            "https://github.com/owner/name"
        );
        // The SSH port is dropped.
        assert_eq!(
            url("ssh://git@gitea.example.test:2222/owner/name"),
            "https://gitea.example.test/owner/name"
        );
        assert_eq!(
            url("ssh://github.com/owner/name"),
            "https://github.com/owner/name"
        );
    }

    #[test]
    fn derives_the_name_from_the_whole_path() {
        assert_eq!(ok("https://gitlab.com/org/team/api").1, "org/team/api");
        assert_eq!(ok("git@gitlab.com:org/team/api.git").1, "org/team/api");
    }

    #[test]
    fn removes_a_trailing_git_and_slash() {
        for input in [
            "https://github.com/owner/name",
            "https://github.com/owner/name/",
            "https://github.com/owner/name.git",
            "https://github.com/owner/name.git/",
            " https://github.com/owner/name ",
        ] {
            assert_eq!(url(input), "https://github.com/owner/name", "{input:?}");
        }
    }

    #[test]
    fn every_form_of_a_repository_gives_the_same_url() {
        let forms = [
            "https://github.com/owner/name",
            "https://GITHUB.com/owner/name.git",
            "git@github.com:owner/name.git",
            "ssh://git@github.com/owner/name",
            "ssh://git@github.com:22/owner/name/",
        ];
        for form in forms {
            assert_eq!(url(form), "https://github.com/owner/name", "{form:?}");
        }
        // The path keeps its case.
        assert_ne!(url("https://github.com/Owner/Name"), url(forms[0]));
    }

    #[test]
    fn rejects_other_forms() {
        use InvalidRepositoryUrl::*;
        let cases = [
            ("http://github.com/owner/name", UnsupportedForm),
            ("file:///srv/git/name.git", UnsupportedForm),
            ("git://github.com/owner/name", UnsupportedForm),
            ("/srv/git/owner/name", UnsupportedForm),
            ("../owner/name", UnsupportedForm),
            ("C:\\repos\\name", UnsupportedForm),
            ("github.com:owner/name", UnsupportedForm),
            ("owner/name", UnsupportedForm),
            ("", UnsupportedForm),
            ("https://github.com/owner name/x", UnsupportedForm),
            ("https://github.com/owner", InvalidPath),
            ("https://github.com/owner/", InvalidPath),
            ("https://github.com/owner/.git", InvalidPath),
            ("https://github.com", InvalidPath),
            ("https://github.com/owner//name", InvalidPath),
            ("https://github.com/owner/../name", InvalidPath),
            ("git@github.com:owner", InvalidPath),
            ("https://user:token@github.com/owner/name", ExtraParts),
            ("https://github.com/owner/name?tab=readme", ExtraParts),
            ("https://github.com/owner/name#readme", ExtraParts),
            ("https:///owner/name", InvalidHost),
            ("https://git_hub.com/owner/name", InvalidHost),
            ("https://github.com:x/owner/name", InvalidHost),
            ("git@:owner/name", InvalidHost),
        ];
        for (input, error) in cases {
            assert_eq!(normalise(input), Err(error), "{input:?}");
        }
        assert_eq!(
            normalise(&format!("https://github.com/o/{}", "n".repeat(MAX_LEN))),
            Err(TooLong)
        );
    }
}
