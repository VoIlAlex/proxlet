//! The immutable snapshot of rules and settings the engine serves from.

use std::path::PathBuf;

use http::uri::{Authority, Scheme};
use proxlet_core::{Config, RuleTable, Settings, Target};

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
}

/// A files rule, ready to serve.
#[derive(Debug)]
pub(crate) struct FilesTarget {
    /// The rule's route as the user wrote it, for error messages.
    pub route: String,
    /// Canonical root directory (or the configured path if it cannot be resolved).
    pub root: PathBuf,
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
                    })
                }
                Target::Directory(dir) => {
                    let root = dir.canonicalize().unwrap_or_else(|e| {
                        tracing::warn!(route, dir = %dir.display(), "files directory unavailable: {e}");
                        dir.clone()
                    });
                    Handler::Files(FilesTarget { route, root })
                }
            };
            entries.push((rule.route.clone(), handler));
        }
        Ok(Self {
            table: RuleTable::build(entries),
            settings: config.settings.clone(),
        })
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
