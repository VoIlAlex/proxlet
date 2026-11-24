//! What proxlet needs to know about a front server's config: its server blocks,
//! the ports and names they claim, and where proxlet may write.

use std::fmt;
use std::path::PathBuf;

/// A place in a config file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    /// The file.
    pub file: PathBuf,
    /// 1-based line.
    pub line: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file.display(), self.line)
    }
}

/// One `listen` directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    /// The port it listens on.
    pub port: u16,
    /// Whether the address is IPv6 (`[::]:80`).
    pub ipv6: bool,
    /// The default-server flag as written (`default_server` or `default`), if set.
    pub default_flag: Option<String>,
    /// Where the directive is.
    pub at: Location,
}

/// One `server_name` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerName {
    /// The name as written: exact, `*.wild`, `.wild`, `wild.*` or `~regex`.
    pub name: String,
    /// Where the directive is.
    pub at: Location,
}

/// One `server` block in the `http` context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerBlock {
    /// Its `listen` directives. Empty means the default, port 80.
    pub listens: Vec<Listen>,
    /// Its `server_name` entries.
    pub names: Vec<ServerName>,
    /// Where the block starts.
    pub at: Location,
    /// Whether proxlet wrote it.
    pub managed: bool,
}

impl ServerBlock {
    /// Whether the block serves `port`.
    #[must_use]
    pub fn listens_on(&self, port: u16) -> bool {
        if self.listens.is_empty() {
            return port == 80;
        }
        self.listens.iter().any(|l| l.port == port)
    }
}

/// An `include` with a wildcard directly in the `http` context: a directory
/// whose matching files nginx loads as part of `http`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeDir {
    /// The directory.
    pub dir: PathBuf,
    /// The file-name pattern, e.g. `*.conf` or `*`.
    pub pattern: String,
    /// Where the `include` is.
    pub at: Location,
}

impl IncludeDir {
    /// The file name proxlet uses in this directory, matching the pattern.
    #[must_use]
    pub fn file_name(&self) -> String {
        match self.pattern.strip_prefix('*') {
            Some(suffix) => format!("proxlet{suffix}"),
            None => "proxlet.conf".into(),
        }
    }
}

/// The parts of a front server's config proxlet uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontConfig {
    /// The main config file.
    pub conf_path: PathBuf,
    /// Server blocks in the `http` context, in load order.
    pub servers: Vec<ServerBlock>,
    /// Wildcard includes in the `http` context, in load order.
    pub http_includes: Vec<IncludeDir>,
    /// The main-context `error_log` path as written, if any.
    pub error_log: Option<String>,
    /// The `user` directive's user, which the worker processes run as.
    pub worker_user: Option<String>,
    /// The `user` directive's group, if it names one.
    pub worker_group: Option<String>,
}

impl FrontConfig {
    /// The server name (not proxlet's own) that serves `host` on `port`, using
    /// nginx's precedence: exact name, then the longest leading wildcard, then
    /// the longest trailing wildcard, then the first matching regex.
    #[must_use]
    pub fn claims(&self, host: &str, port: u16) -> Option<&ServerName> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let names = || {
            self.servers
                .iter()
                .filter(|s| !s.managed && s.listens_on(port))
                .flat_map(|s| s.names.iter())
        };
        if let Some(n) =
            names().find(|n| !n.name.starts_with('~') && n.name.eq_ignore_ascii_case(&host))
        {
            return Some(n);
        }
        let leading = names()
            .filter_map(|n| leading_wildcard(&n.name, &host).map(|len| (len, n)))
            .max_by_key(|(len, _)| *len);
        if let Some((_, n)) = leading {
            return Some(n);
        }
        let trailing = names()
            .filter_map(|n| trailing_wildcard(&n.name, &host).map(|len| (len, n)))
            .max_by_key(|(len, _)| *len);
        if let Some((_, n)) = trailing {
            return Some(n);
        }
        names().find(|n| regex_matches(&n.name, &host))
    }

    /// The `listen` directive (not proxlet's own) marked as the default server for `port`.
    #[must_use]
    pub fn default_server(&self, port: u16) -> Option<&Listen> {
        self.servers
            .iter()
            .filter(|s| !s.managed)
            .flat_map(|s| s.listens.iter())
            .find(|l| l.port == port && l.default_flag.is_some())
    }

    /// Whether any server block, including proxlet's, listens on `port`.
    #[must_use]
    pub fn listens_on(&self, port: u16) -> bool {
        self.servers.iter().any(|s| s.listens_on(port))
    }

    /// Whether proxlet's own block is loaded.
    #[must_use]
    pub fn has_managed_block(&self) -> bool {
        self.servers.iter().any(|s| s.managed)
    }
}

/// `*.example.com` or `.example.com`: the matched suffix length.
fn leading_wildcard(name: &str, host: &str) -> Option<usize> {
    let lower = name.to_ascii_lowercase();
    if let Some(suffix) = lower.strip_prefix("*.") {
        return host
            .strip_suffix(suffix)
            .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.'))
            .then_some(suffix.len());
    }
    if let Some(suffix) = lower.strip_prefix('.') {
        let matches = host == suffix
            || host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('.'));
        return matches.then_some(suffix.len());
    }
    None
}

/// `www.example.*`: the matched prefix length.
fn trailing_wildcard(name: &str, host: &str) -> Option<usize> {
    let prefix = name.to_ascii_lowercase();
    let prefix = prefix.strip_suffix(".*")?;
    host.strip_prefix(prefix)
        .is_some_and(|rest| rest.len() > 1 && rest.starts_with('.'))
        .then_some(prefix.len())
}

/// `~regex`, matched like PCRE where the `regex` crate agrees. A pattern it
/// cannot compile never matches rather than failing the whole check.
fn regex_matches(name: &str, host: &str) -> bool {
    name.strip_prefix('~')
        .and_then(|pattern| regex::Regex::new(pattern).ok())
        .is_some_and(|re| re.is_match(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize) -> Location {
        Location {
            file: "/etc/nginx/sites-enabled/site".into(),
            line,
        }
    }

    fn server(port: u16, default: bool, names: &[&str], managed: bool) -> ServerBlock {
        ServerBlock {
            listens: vec![Listen {
                port,
                ipv6: false,
                default_flag: default.then(|| "default_server".into()),
                at: at(2),
            }],
            names: names
                .iter()
                .enumerate()
                .map(|(i, n)| ServerName {
                    name: (*n).into(),
                    at: at(10 + i),
                })
                .collect(),
            at: at(1),
            managed,
        }
    }

    fn cfg(servers: Vec<ServerBlock>) -> FrontConfig {
        FrontConfig {
            servers,
            ..FrontConfig::default()
        }
    }

    fn claim<'a>(c: &'a FrontConfig, host: &str) -> Option<&'a str> {
        c.claims(host, 80).map(|n| n.name.as_str())
    }

    #[test]
    fn exact_names_are_case_insensitive() {
        let c = cfg(vec![server(80, false, &["Site.Example"], false)]);
        assert_eq!(claim(&c, "site.example"), Some("Site.Example"));
        assert_eq!(claim(&c, "SITE.example."), Some("Site.Example"));
        assert_eq!(claim(&c, "other.example"), None);
    }

    #[test]
    fn only_the_port_counts() {
        let c = cfg(vec![server(8080, false, &["site.example"], false)]);
        assert_eq!(claim(&c, "site.example"), None);
        assert!(c.claims("site.example", 8080).is_some());
    }

    #[test]
    fn a_server_without_listen_means_port_80() {
        let mut s = server(80, false, &["site.example"], false);
        s.listens.clear();
        assert_eq!(claim(&cfg(vec![s]), "site.example"), Some("site.example"));
    }

    #[test]
    fn leading_wildcards() {
        let c = cfg(vec![server(
            80,
            false,
            &["*.example.com", "*.api.example.com", ".dot.example"],
            false,
        )]);
        assert_eq!(claim(&c, "a.example.com"), Some("*.example.com"));
        assert_eq!(claim(&c, "x.api.example.com"), Some("*.api.example.com"));
        assert_eq!(claim(&c, "example.com"), None);
        assert_eq!(claim(&c, "dot.example"), Some(".dot.example"));
        assert_eq!(claim(&c, "a.b.dot.example"), Some(".dot.example"));
        assert_eq!(claim(&c, "notdot.example"), None);
    }

    #[test]
    fn trailing_wildcards() {
        let c = cfg(vec![server(80, false, &["www.example.*"], false)]);
        assert_eq!(claim(&c, "www.example.org"), Some("www.example.*"));
        assert_eq!(claim(&c, "www.example"), None);
    }

    #[test]
    fn precedence_is_exact_then_leading_then_trailing_then_regex() {
        let c = cfg(vec![
            server(80, false, &["~^www\\..+$"], false),
            server(80, false, &["www.example.*"], false),
            server(80, false, &["*.example.com"], false),
            server(80, false, &["www.example.com"], false),
        ]);
        assert_eq!(claim(&c, "www.example.com"), Some("www.example.com"));
        assert_eq!(claim(&c, "api.example.com"), Some("*.example.com"));
        assert_eq!(claim(&c, "www.example.org"), Some("www.example.*"));
        assert_eq!(claim(&c, "www.other.net"), Some("~^www\\..+$"));
    }

    #[test]
    fn regex_names() {
        let c = cfg(vec![server(
            80,
            false,
            &["~^(?<sub>.+)\\.dev\\.test$", "~(unclosed"],
            false,
        )]);
        assert_eq!(
            claim(&c, "app.dev.test"),
            Some("~^(?<sub>.+)\\.dev\\.test$")
        );
        // The uncompilable pattern is skipped, not an error.
        assert_eq!(claim(&c, "unclosed"), None);
    }

    #[test]
    fn proxlets_own_block_never_counts() {
        let c = cfg(vec![server(80, true, &["_"], true)]);
        assert_eq!(claim(&c, "_"), None);
        assert!(c.default_server(80).is_none());
        assert!(c.has_managed_block());
    }

    #[test]
    fn default_server_by_port() {
        let c = cfg(vec![
            server(80, false, &["a.example"], false),
            server(80, true, &["_"], false),
        ]);
        assert_eq!(c.default_server(80).unwrap().at.line, 2);
        assert!(c.default_server(8080).is_none());
    }

    #[test]
    fn include_file_names_follow_the_pattern() {
        let dir = |pattern: &str| IncludeDir {
            dir: "/etc/nginx/conf.d".into(),
            pattern: pattern.into(),
            at: at(1),
        };
        assert_eq!(dir("*.conf").file_name(), "proxlet.conf");
        assert_eq!(dir("*").file_name(), "proxlet");
    }
}
