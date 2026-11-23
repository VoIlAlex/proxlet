//! Routes: where requests come in (`/path` or `host/path`).

use std::fmt;
use std::str::FromStr;

/// Why a route could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// The route was empty.
    #[error("the route is empty; use `/path` or `host/path`")]
    Empty,
    /// The host part contains characters that cannot appear in a host name.
    #[error(
        "`{0}` is not a valid host name; use letters, digits, `-` and `.` (no port, no scheme)"
    )]
    InvalidHost(String),
    /// The path part contains characters proxlet does not accept in a route.
    #[error("`{0}` is not a valid path; it cannot contain spaces, `?`, `#`, `\\` or `..` segments")]
    InvalidPath(String),
}

/// A route: an optional host and a normalised path.
///
/// Hosts are lower case. Paths always start with `/`, never end with `/` (except
/// the root path) and never contain empty or `..` segments.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Route {
    host: Option<String>,
    path: String,
}

impl Route {
    /// The host this route is limited to, or `None` for any host.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// The normalised path prefix.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether `request_path` falls under this route's path on a segment boundary.
    #[must_use]
    pub fn path_matches(&self, request_path: &str) -> bool {
        path_has_prefix(request_path, &self.path)
    }
}

/// Whether `path` is `prefix` or lies under it on a segment boundary.
#[must_use]
pub fn path_has_prefix(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

impl FromStr for Route {
    type Err = RouteError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(RouteError::Empty);
        }
        let (host, raw_path) = match input.find('/') {
            Some(0) => (None, input),
            Some(i) => (Some(&input[..i]), &input[i..]),
            None => (Some(input), "/"),
        };
        let host = host.map(normalise_host).transpose()?;
        let path = normalise_path(raw_path)?;
        Ok(Self { host, path })
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Some(host) if self.path == "/" => f.write_str(host),
            Some(host) => write!(f, "{host}{}", self.path),
            None => f.write_str(&self.path),
        }
    }
}

/// Normalises a host name: lower case, no trailing dot. Returns `None` if invalid.
#[must_use]
pub fn normalise_host_name(raw: &str) -> Option<String> {
    let host = raw.trim_end_matches('.').to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    valid.then_some(host)
}

fn normalise_host(raw: &str) -> Result<String, RouteError> {
    normalise_host_name(raw).ok_or_else(|| RouteError::InvalidHost(raw.to_owned()))
}

fn normalise_path(raw: &str) -> Result<String, RouteError> {
    let invalid = || RouteError::InvalidPath(raw.to_owned());
    if raw
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '?' | '#' | '\\'))
    {
        return Err(invalid());
    }
    let mut path = String::with_capacity(raw.len());
    for segment in raw.split('/').filter(|s| !s.is_empty()) {
        if segment == ".." || segment == "." {
            return Err(invalid());
        }
        path.push('/');
        path.push_str(segment);
    }
    if path.is_empty() {
        path.push('/');
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Route {
        s.parse().unwrap()
    }

    #[test]
    fn path_only_route() {
        let r = parse("/api");
        assert_eq!(r.host(), None);
        assert_eq!(r.path(), "/api");
    }

    #[test]
    fn host_route_is_lowercased_and_trailing_slash_dropped() {
        let r = parse("Example.com/api/");
        assert_eq!(r.host(), Some("example.com"));
        assert_eq!(r.path(), "/api");
        assert_eq!(r.to_string(), "example.com/api");
    }

    #[test]
    fn bare_host_means_root() {
        let r = parse("example.com");
        assert_eq!(r.host(), Some("example.com"));
        assert_eq!(r.path(), "/");
        assert_eq!(r.to_string(), "example.com");
        assert_eq!(parse("example.com/"), r);
    }

    #[test]
    fn root_path_is_kept() {
        assert_eq!(parse("/").path(), "/");
        assert_eq!(parse("//").path(), "/");
    }

    #[test]
    fn duplicate_slashes_collapse() {
        assert_eq!(parse("/a//b/").path(), "/a/b");
    }

    #[test]
    fn invalid_routes_are_rejected() {
        for bad in [
            "exa mple.com/api",
            "",
            "   ",
            "example.com:8080/api",
            "http://example.com/api",
            "/a/../b",
            "/a?b",
            "/a#b",
            "-bad.com",
        ] {
            assert!(bad.parse::<Route>().is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn segment_boundary_matching() {
        let r = parse("/api");
        assert!(r.path_matches("/api"));
        assert!(r.path_matches("/api/users"));
        assert!(!r.path_matches("/apix"));
        assert!(!r.path_matches("/"));
        assert!(parse("/").path_matches("/anything"));
    }
}
