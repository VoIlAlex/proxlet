//! Targets: where matched requests go (an upstream server or a directory).

use std::fmt;
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};

/// Why a target could not be parsed or validated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    /// The target was empty.
    #[error("the target is empty; use a port (`8000`), `host:port`, a URL or a directory (`.`)")]
    Empty,
    /// A numeric target outside the port range.
    #[error("`{0}` is not a valid port; ports go from 1 to 65535")]
    PortOutOfRange(String),
    /// An upstream whose host part is not valid.
    #[error("`{0}` is not a valid upstream host")]
    InvalidHost(String),
    /// An upstream URL that carries a path, query or credentials.
    #[error(
        "`{0}`: upstream URLs cannot include a path, query or credentials yet; use `scheme://host:port`"
    )]
    UnsupportedUrl(String),
    /// A directory target that does not exist.
    #[error("`{0}` does not exist")]
    NotFound(PathBuf),
    /// A directory target that is a file or something else.
    #[error("`{0}` is not a directory")]
    NotADirectory(PathBuf),
    /// A directory target proxlet cannot read.
    #[error("`{0}` cannot be read: {1}")]
    NotReadable(PathBuf, String),
}

/// The scheme used to reach an upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS.
    Https,
}

impl Scheme {
    /// The scheme as it appears in a URL.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// An upstream server: scheme, host and port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Upstream {
    /// How to talk to the upstream.
    pub scheme: Scheme,
    /// Host name or IP address (IPv6 without brackets).
    pub host: String,
    /// TCP port.
    pub port: u16,
}

impl Upstream {
    /// `host:port`, with brackets around IPv6 addresses.
    #[must_use]
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The upstream as a URL without a path, e.g. `http://127.0.0.1:8000`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("{}://{}", self.scheme.as_str(), self.authority())
    }
}

/// Where a rule sends matched requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Proxy to an upstream server.
    Upstream(Upstream),
    /// Serve files from an absolute directory path.
    Directory(PathBuf),
}

impl Target {
    /// Parses a command-line target. Relative directories resolve against `cwd`.
    ///
    /// The order is: a bare number is a local port; `host:port` or an
    /// `http(s)://` URL is an upstream; anything else is a directory.
    pub fn parse(input: &str, cwd: &Path) -> Result<Self, TargetError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(TargetError::Empty);
        }
        if input.bytes().all(|b| b.is_ascii_digit()) {
            let port = parse_port(input)?;
            return Ok(Self::Upstream(Upstream {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port,
            }));
        }
        for (prefix, scheme) in [("http://", Scheme::Http), ("https://", Scheme::Https)] {
            if let Some(rest) = strip_prefix_ignore_case(input, prefix) {
                let authority = rest.strip_suffix('/').unwrap_or(rest);
                if authority.contains(['/', '?', '#', '@']) {
                    return Err(TargetError::UnsupportedUrl(input.to_owned()));
                }
                return parse_authority(authority, scheme, true).map(Self::Upstream);
            }
        }
        if let Ok(upstream) = parse_authority(input, Scheme::Http, false) {
            return Ok(Self::Upstream(upstream));
        }
        Ok(Self::Directory(cwd.join(input)))
    }

    /// Reads a target as stored in the config file: an absolute directory or a URL.
    pub fn from_stored_proxy(url: &str) -> Result<Self, TargetError> {
        match Self::parse(url, Path::new("/"))? {
            t @ Self::Upstream(_) => Ok(t),
            Self::Directory(_) => Err(TargetError::InvalidHost(url.to_owned())),
        }
    }

    /// Checks the target can be served and returns it in canonical form.
    ///
    /// Directories must exist, be directories and be readable; they are
    /// returned canonicalised. Upstreams are returned unchanged: whether they
    /// are listening is a warning, not an error (see [`crate::port::is_listening`]).
    pub fn validate(self) -> Result<Self, TargetError> {
        match self {
            Self::Upstream(_) => Ok(self),
            Self::Directory(path) => {
                let canonical = path.canonicalize().map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        TargetError::NotFound(path.clone())
                    } else {
                        TargetError::NotReadable(path.clone(), e.to_string())
                    }
                })?;
                if !canonical.is_dir() {
                    return Err(TargetError::NotADirectory(path));
                }
                std::fs::read_dir(&canonical)
                    .map_err(|e| TargetError::NotReadable(path.clone(), e.to_string()))?;
                Ok(Self::Directory(canonical))
            }
        }
    }

    /// `proxy` or `files`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Upstream(_) => "proxy",
            Self::Directory(_) => "files",
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upstream(u) => f.write_str(&u.url()),
            Self::Directory(p) => write!(f, "{}", p.display()),
        }
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn parse_port(s: &str) -> Result<u16, TargetError> {
    match s.parse::<u32>() {
        Ok(n) if (1..=65535).contains(&n) => Ok(u16::try_from(n).expect("checked range")),
        _ => Err(TargetError::PortOutOfRange(s.to_owned())),
    }
}

/// Parses `host[:port]`. The port is optional only when `port_optional` is set.
fn parse_authority(s: &str, scheme: Scheme, port_optional: bool) -> Result<Upstream, TargetError> {
    let invalid = || TargetError::InvalidHost(s.to_owned());
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (ip, after) = rest.split_once(']').ok_or_else(invalid)?;
        ip.parse::<Ipv6Addr>().map_err(|_| invalid())?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(invalid()),
        };
        (ip.to_owned(), port)
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (s.to_owned(), None),
        }
    };
    let port = match port {
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => parse_port(p)?,
        None if port_optional => scheme.default_port(),
        Some(_) | None => return Err(invalid()),
    };
    let host = if host.contains(':') {
        host
    } else {
        crate::route::normalise_host_name(&host).ok_or_else(invalid)?
    };
    Ok(Upstream { scheme, host, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(scheme: Scheme, host: &str, port: u16) -> Target {
        Target::Upstream(Upstream {
            scheme,
            host: host.into(),
            port,
        })
    }

    fn parse(s: &str) -> Result<Target, TargetError> {
        Target::parse(s, Path::new("/home/me/site"))
    }

    #[test]
    fn port_target() {
        let t = parse("8000").unwrap();
        assert_eq!(t, up(Scheme::Http, "127.0.0.1", 8000));
        assert_eq!(t.to_string(), "http://127.0.0.1:8000");
    }

    #[test]
    fn port_out_of_range() {
        for bad in ["0", "65536", "99999999999"] {
            assert!(
                matches!(parse(bad), Err(TargetError::PortOutOfRange(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn host_port_target() {
        assert_eq!(
            parse("10.0.0.5:9000").unwrap(),
            up(Scheme::Http, "10.0.0.5", 9000)
        );
        assert_eq!(
            parse("Api.Local:9000").unwrap(),
            up(Scheme::Http, "api.local", 9000)
        );
        assert_eq!(parse("[::1]:9000").unwrap(), up(Scheme::Http, "::1", 9000));
    }

    #[test]
    fn url_targets() {
        assert_eq!(
            parse("http://10.0.0.5:9000").unwrap(),
            up(Scheme::Http, "10.0.0.5", 9000)
        );
        assert_eq!(
            parse("https://api.example.com/").unwrap(),
            up(Scheme::Https, "api.example.com", 443)
        );
        assert_eq!(
            parse("HTTP://localhost").unwrap(),
            up(Scheme::Http, "localhost", 80)
        );
        assert!(matches!(
            parse("http://host:1/base"),
            Err(TargetError::UnsupportedUrl(_))
        ));
    }

    #[test]
    fn directory_targets_resolve_against_cwd() {
        assert_eq!(
            parse(".").unwrap(),
            Target::Directory("/home/me/site/.".into())
        );
        assert_eq!(
            parse("./8000").unwrap(),
            Target::Directory("/home/me/site/./8000".into())
        );
        assert_eq!(parse("/srv").unwrap(), Target::Directory("/srv".into()));
    }

    #[test]
    fn validate_directory() {
        let dir = tempfile::tempdir().unwrap();
        let t = Target::parse(".", dir.path()).unwrap().validate().unwrap();
        assert_eq!(t, Target::Directory(dir.path().canonicalize().unwrap()));
    }

    #[test]
    fn validate_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = Target::parse("./nope", dir.path())
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(matches!(err, TargetError::NotFound(_)));
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn validate_file_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "x").unwrap();
        let err = Target::parse("f", dir.path())
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(matches!(err, TargetError::NotADirectory(_)));
    }

    #[test]
    fn numeric_directory_name_with_dot_slash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("8000")).unwrap();
        let t = Target::parse("./8000", dir.path())
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(
            t,
            Target::Directory(dir.path().join("8000").canonicalize().unwrap())
        );
    }
}
