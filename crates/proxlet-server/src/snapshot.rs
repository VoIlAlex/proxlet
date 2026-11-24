//! The immutable snapshot of rules and settings the engine serves from.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use http::uri::{Authority, Scheme};
use proxlet_core::{Config, RuleTable, Settings, Target};

use crate::stats::RuleStats;

/// Why a configuration could not be turned into a snapshot.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// An upstream address could not be used as a URI authority.
    #[error("rule `{route}`: `{upstream}` is not a usable upstream address")]
    BadUpstream {
        /// The rule's route.
        route: String,
        /// The upstream as written.
        upstream: String,
    },
}

/// Everything a request needs, built once per configuration change.
#[derive(Debug)]
pub struct Snapshot {
    pub(crate) table: RuleTable<Handler>,
    pub(crate) settings: Settings,
    /// Statistics of requests no rule answered.
    pub(crate) unmatched: Arc<RuleStats>,
}

/// How a matched rule is served.
#[derive(Debug)]
pub(crate) enum Handler {
    Proxy(ProxyTarget),
    Files(FilesTarget),
}

/// A proxy rule, ready to forward.
#[derive(Debug)]
pub(crate) struct ProxyTarget {
    /// The rule's route as the user wrote it, for error messages.
    pub route: String,
    pub scheme: Scheme,
    pub authority: Authority,
    pub stats: Arc<RuleStats>,
}

/// A files rule, ready to serve.
#[derive(Debug)]
pub(crate) struct FilesTarget {
    /// The rule's route as the user wrote it, for error messages.
    pub route: String,
    /// Canonical root directory (or the configured path if it cannot be resolved).
    pub root: PathBuf,
    pub stats: Arc<RuleStats>,
}

impl Handler {
    pub(crate) fn route(&self) -> &str {
        match self {
            Self::Proxy(t) => &t.route,
            Self::Files(t) => &t.route,
        }
    }

    pub(crate) fn target(&self) -> String {
        match self {
            Self::Proxy(t) => t.authority.to_string(),
            Self::Files(t) => t.root.display().to_string(),
        }
    }

    pub(crate) fn stats(&self) -> &Arc<RuleStats> {
        match self {
            Self::Proxy(t) => &t.stats,
            Self::Files(t) => &t.stats,
        }
    }

    fn stats_mut(&mut self) -> &mut Arc<RuleStats> {
        match self {
            Self::Proxy(t) => &mut t.stats,
            Self::Files(t) => &mut t.stats,
        }
    }
}

/// One rule's statistics, read at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct StatsRow {
    /// The rule's route; `None` for the "no rule" row.
    pub rule: Option<String>,
    /// The upstream `host:port` or served directory.
    pub target: Option<String>,
    /// The numbers.
    pub totals: crate::Totals,
}

impl Snapshot {
    /// Builds a snapshot from a configuration.
    ///
    /// A files directory that cannot be resolved is not an error here: the
    /// rule keeps its configured path and answers `404` until it exists. The
    /// CLI validates directories when rules are added.
    pub fn build(config: &Config) -> Result<Self, BuildError> {
        let mut entries = Vec::with_capacity(config.rules.len());
        for rule in &config.rules {
            let route = rule.route.to_string();
            let handler = match &rule.target {
                Target::Upstream(u) => {
                    let authority = u.authority().parse::<Authority>().map_err(|_| {
                        BuildError::BadUpstream {
                            route: route.clone(),
                            upstream: u.url(),
                        }
                    })?;
                    let scheme = match u.scheme {
                        proxlet_core::Scheme::Http => Scheme::HTTP,
                        proxlet_core::Scheme::Https => Scheme::HTTPS,
                    };
                    Handler::Proxy(ProxyTarget {
                        route,
                        scheme,
                        authority,
                        stats: Arc::default(),
                    })
                }
                Target::Directory(dir) => {
                    let root = dir.canonicalize().unwrap_or_else(|e| {
                        tracing::warn!(route, dir = %dir.display(), "files directory unavailable: {e}");
                        dir.clone()
                    });
                    Handler::Files(FilesTarget {
                        route,
                        root,
                        stats: Arc::default(),
                    })
                }
            };
            entries.push((rule.route.clone(), handler));
        }
        Ok(Self {
            table: RuleTable::build(entries),
            settings: config.settings.clone(),
            unmatched: Arc::default(),
        })
    }

    /// Takes over `old`'s statistics for every route that is still here, and
    /// for the "no rule" row (design decision 1).
    pub(crate) fn adopt_stats(&mut self, old: &Snapshot) {
        let by_route: HashMap<&str, &Arc<RuleStats>> =
            old.table.values().map(|h| (h.route(), h.stats())).collect();
        for handler in self.table.values_mut() {
            if let Some(stats) = by_route.get(handler.route()) {
                *handler.stats_mut() = Arc::clone(stats);
            }
        }
        self.unmatched = Arc::clone(&old.unmatched);
    }

    /// Every rule's statistics in `proxlet ls` order (rules without a host
    /// first, then by host and path), and the "no rule" row last.
    #[must_use]
    pub fn stats(&self) -> Vec<StatsRow> {
        let mut rows: Vec<StatsRow> = self
            .table
            .values()
            .map(|h| StatsRow {
                rule: Some(h.route().to_owned()),
                target: Some(h.target()),
                totals: h.stats().read(),
            })
            .collect();
        rows.sort_by_cached_key(|r| ls_order(r.rule.as_deref().unwrap_or_default()));
        rows.push(StatsRow {
            rule: None,
            target: None,
            totals: self.unmatched.read(),
        });
        rows
    }

    /// The settings this snapshot serves with.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Number of rules.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.table.len()
    }
}

/// `proxlet ls` order for a route's text: rules without a host first, then by
/// host and path.
fn ls_order(route: &str) -> (bool, String, String) {
    if route.starts_with('/') {
        return (false, String::new(), route.to_owned());
    }
    let (host, path) = route.split_at(route.find('/').unwrap_or(route.len()));
    (true, host.to_owned(), path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn config(rules: &[(&str, &str)]) -> Config {
        let mut c = Config::default();
        for (route, target) in rules {
            c.rules.push(proxlet_core::Rule {
                route: route.parse().unwrap(),
                target: Target::parse(target, Path::new("/")).unwrap(),
            });
        }
        c
    }

    fn stats_of<'a>(s: &'a Snapshot, route: &str) -> &'a Arc<RuleStats> {
        s.table
            .values()
            .find(|h| h.route() == route)
            .unwrap()
            .stats()
    }

    #[test]
    fn statistics_follow_routes_across_snapshots() {
        let old = Snapshot::build(&config(&[
            ("/api", "8000"),
            ("/", "3000"),
            ("/gone", "4000"),
        ]))
        .unwrap();
        let mut new = Snapshot::build(&config(&[
            ("/api", "9000"),
            ("/", "3000"),
            ("/docs", "/tmp"),
        ]))
        .unwrap();
        new.adopt_stats(&old);
        assert!(
            Arc::ptr_eq(stats_of(&old, "/api"), stats_of(&new, "/api")),
            "target change keeps them"
        );
        assert!(Arc::ptr_eq(stats_of(&old, "/"), stats_of(&new, "/")));
        assert!(Arc::ptr_eq(&old.unmatched, &new.unmatched));
        assert!(!Arc::ptr_eq(
            stats_of(&old, "/gone"),
            stats_of(&new, "/docs")
        ));

        // Removed, then added again: fresh.
        let mut without = Snapshot::build(&config(&[("/", "3000")])).unwrap();
        without.adopt_stats(&new);
        let mut again = Snapshot::build(&config(&[("/", "3000"), ("/api", "9000")])).unwrap();
        again.adopt_stats(&without);
        assert!(!Arc::ptr_eq(
            stats_of(&new, "/api"),
            stats_of(&again, "/api")
        ));
    }

    #[test]
    fn rows_are_in_ls_order_with_no_rule_last() {
        let s = Snapshot::build(&config(&[
            ("example.com/", "3000"),
            ("/api", "8000"),
            ("/", "3000"),
            ("a.example/x", "3000"),
        ]))
        .unwrap();
        let rules: Vec<Option<String>> = s.stats().into_iter().map(|r| r.rule).collect();
        assert_eq!(
            rules,
            [
                Some("/".to_owned()),
                Some("/api".to_owned()),
                Some("a.example/x".to_owned()),
                Some("example.com".to_owned()),
                None
            ]
        );
    }
}
