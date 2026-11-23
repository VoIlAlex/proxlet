//! The immutable rule table used on the request path.
//!
//! Lookup is a hash probe on the host plus a short scan from the longest path
//! prefix to the shortest. Rules for the request's exact host are tried before
//! rules with no host.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::route::{Route, path_has_prefix};

/// A rule table mapping routes to a per-rule payload `T`.
#[derive(Debug)]
pub struct RuleTable<T> {
    by_host: HashMap<String, Vec<(String, T)>>,
    any_host: Vec<(String, T)>,
    len: usize,
}

impl<T> Default for RuleTable<T> {
    fn default() -> Self {
        Self {
            by_host: HashMap::new(),
            any_host: Vec::new(),
            len: 0,
        }
    }
}

/// A successful lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match<'a, T> {
    /// The matched rule's path prefix.
    pub prefix: &'a str,
    /// The matched rule's payload.
    pub value: &'a T,
}

impl<T> RuleTable<T> {
    /// Builds a table. Routes are assumed unique (the config guarantees it).
    pub fn build(entries: impl IntoIterator<Item = (Route, T)>) -> Self {
        let mut table = Self::default();
        for (route, value) in entries {
            let entry = (route.path().to_owned(), value);
            match route.host() {
                Some(h) => table.by_host.entry(h.to_owned()).or_default().push(entry),
                None => table.any_host.push(entry),
            }
            table.len += 1;
        }
        let by_len = |a: &(String, T), b: &(String, T)| b.0.len().cmp(&a.0.len());
        table.any_host.sort_by(by_len);
        for list in table.by_host.values_mut() {
            list.sort_by(by_len);
        }
        table
    }

    /// Number of rules.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the table has no rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Finds the rule serving `path` on `host` (a raw `Host` header value or
    /// authority: case, port and trailing dot are ignored).
    #[must_use]
    pub fn lookup(&self, host: Option<&str>, path: &str) -> Option<Match<'_, T>> {
        let host_rules = host
            .and_then(normalise_request_host)
            .and_then(|h| self.by_host.get(h.as_ref()));
        host_rules
            .and_then(|list| longest(list, path))
            .or_else(|| longest(&self.any_host, path))
    }
}

fn longest<'a, T>(list: &'a [(String, T)], path: &str) -> Option<Match<'a, T>> {
    list.iter()
        .find(|(prefix, _)| path_has_prefix(path, prefix))
        .map(|(prefix, value)| Match { prefix, value })
}

/// Normalises a `Host` header or URI authority to a bare lower-case host.
#[must_use]
pub fn normalise_request_host(raw: &str) -> Option<Cow<'_, str>> {
    let host = if let Some(rest) = raw.strip_prefix('[') {
        &rest[..rest.find(']')?]
    } else {
        raw.rsplit_once(':').map_or(raw, |(h, _)| h)
    };
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(Cow::Owned(host.to_ascii_lowercase()))
    } else {
        Some(Cow::Borrowed(host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn table(routes: &[&str]) -> RuleTable<String> {
        RuleTable::build(
            routes
                .iter()
                .map(|r| (r.parse::<Route>().unwrap(), (*r).to_owned())),
        )
    }

    fn hit<'a>(t: &'a RuleTable<String>, host: &str, path: &str) -> Option<&'a str> {
        t.lookup(Some(host), path).map(|m| m.value.as_str())
    }

    #[test]
    fn longest_path_wins() {
        let t = table(&["/", "/api"]);
        assert_eq!(hit(&t, "x", "/api/users"), Some("/api"));
        assert_eq!(hit(&t, "x", "/other"), Some("/"));
    }

    #[test]
    fn host_rule_beats_hostless_rule() {
        let t = table(&["/api", "example.com/"]);
        assert_eq!(hit(&t, "example.com", "/api/users"), Some("example.com/"));
    }

    #[test]
    fn fallback_to_hostless_rule() {
        let t = table(&["/api", "example.com/admin"]);
        assert_eq!(hit(&t, "example.com", "/api"), Some("/api"));
        assert_eq!(
            hit(&t, "example.com", "/admin/x"),
            Some("example.com/admin")
        );
    }

    #[test]
    fn segment_boundary() {
        let t = table(&["/api"]);
        assert_eq!(hit(&t, "x", "/apix"), None);
        assert_eq!(hit(&t, "x", "/api"), Some("/api"));
    }

    #[test]
    fn host_header_is_normalised() {
        let t = table(&["example.com/"]);
        assert_eq!(hit(&t, "EXAMPLE.com:8080", "/"), Some("example.com/"));
        assert_eq!(hit(&t, "example.com.", "/"), Some("example.com/"));
        assert_eq!(t.lookup(None, "/"), None);
        let t = table(&["/v6"]);
        assert_eq!(hit(&t, "[::1]:80", "/v6"), Some("/v6"));
    }

    /// The obviously-correct matcher the table must agree with.
    fn naive<'a>(rules: &'a [Route], host: &str, path: &str) -> Option<&'a Route> {
        let best = |want_host: Option<&str>| {
            rules
                .iter()
                .filter(|r| r.host() == want_host && r.path_matches(path))
                .max_by_key(|r| r.path().len())
        };
        best(Some(host)).or_else(|| best(None))
    }

    fn route_strategy() -> impl Strategy<Value = Route> {
        let host = prop::option::of(prop::sample::select(vec!["a.com", "b.com"]));
        let path = prop::collection::vec(prop::sample::select(vec!["x", "y", "xy"]), 0..3);
        (host, path).prop_map(|(h, segs)| {
            let p = format!("/{}", segs.join("/"));
            match h {
                Some(h) => format!("{h}{p}"),
                None => p,
            }
            .parse()
            .unwrap()
        })
    }

    proptest! {
        #[test]
        fn agrees_with_naive_matcher(
            routes in prop::collection::hash_set(route_strategy(), 0..10),
            host in prop::sample::select(vec!["a.com", "b.com", "c.com"]),
            segs in prop::collection::vec(prop::sample::select(vec!["x", "y", "xy", "z"]), 0..4),
        ) {
            let routes: Vec<Route> = routes.into_iter().collect();
            let path = format!("/{}", segs.join("/"));
            let t = RuleTable::build(routes.iter().map(|r| (r.clone(), r.clone())));
            let got = t.lookup(Some(host), &path).map(|m| m.value);
            prop_assert_eq!(got, naive(&routes, host, &path));
        }
    }
}
