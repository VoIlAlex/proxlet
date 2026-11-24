//! Operations shared by the CLI commands and the MCP tools. They return data
//! or a [`Failure`] and print nothing (except the front-server warning in the
//! CLI's asking mode), so both front ends behave the same.

use std::path::Path;

use proxlet_core::{
    Change, ConfigFile, Locations, Route, Rule, SettingDef, Settings, Target, Value, settings,
};
use proxlet_front::FrontConfig;

use crate::attach;
use crate::commands::submit;
use crate::control::WireOutcome;
use crate::logs::files;
use crate::logs::read::{self, Follower};
use crate::logs::record::Line;
use crate::logs::show::{self, Selector};
use crate::output::{self, Failure};

/// What a change did, and whether a running server applied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// The outcome.
    pub outcome: WireOutcome,
    /// Whether a running server applied it (false: only the file was written).
    pub live: bool,
}

/// What to do when the front server already serves a rule's host.
pub enum FrontPolicy<'a> {
    /// The CLI: warn, then ask unless `yes`.
    Ask {
        /// `--yes` was given.
        yes: bool,
        /// Asks a yes/no question; `None` without a terminal.
        confirm: &'a dyn Fn(&str) -> Option<bool>,
    },
    /// MCP: refuse unless allowed, never asking.
    Allow(bool),
}

/// A rule that was added or updated.
#[derive(Debug)]
pub struct AddedRule {
    /// The rule's route.
    pub route: Route,
    /// The validated target.
    pub target: Target,
    /// What changed.
    pub changed: Changed,
}

/// Adds or updates a rule, as `proxlet add` does. Relative directories are
/// resolved against `cwd`.
pub fn add_rule(
    locations: &Locations,
    route: &str,
    target: &str,
    cwd: &Path,
    front: &FrontPolicy,
) -> Result<AddedRule, Failure> {
    let raw_target = target;
    let route: Route = route.parse()?;
    let target = Target::parse(target, cwd)?.validate()?;
    check_front_conflict(locations, &route, raw_target, front)?;
    let submitted = submit(
        locations,
        &Change::AddRule(Rule {
            route: route.clone(),
            target: target.clone(),
        }),
    )?;
    Ok(AddedRule {
        route,
        target,
        changed: Changed {
            outcome: submitted.outcome,
            live: submitted.live,
        },
    })
}

/// Removes a rule, as `proxlet rm` does.
pub fn remove_rule(locations: &Locations, route: &str) -> Result<(Route, Changed), Failure> {
    let route: Route = route.parse()?;
    let submitted = submit(locations, &Change::RemoveRule(route.clone()))?;
    Ok((
        route,
        Changed {
            outcome: submitted.outcome,
            live: submitted.live,
        },
    ))
}

/// Changes a setting, as `proxlet config KEY VALUE` does.
pub fn set_setting(
    locations: &Locations,
    key: &str,
    raw: &str,
) -> Result<(&'static SettingDef, Value, Changed), Failure> {
    let def = settings::lookup(key)?;
    let value = Value::parse(def, raw)?;
    let submitted = submit(locations, &Change::SetSetting(def, value.clone()))?;
    Ok((
        def,
        value,
        Changed {
            outcome: submitted.outcome,
            live: submitted.live,
        },
    ))
}

/// Every rule, in `proxlet ls` order: rules without a host first, then by
/// host and path.
pub fn rules(locations: &Locations) -> Result<Vec<Rule>, Failure> {
    let (_, config) = ConfigFile::new(locations.config_file()).read()?;
    let mut rules = config.rules;
    rules.sort_by(|a, b| {
        (a.route.host().is_some(), a.route.host(), a.route.path()).cmp(&(
            b.route.host().is_some(),
            b.route.host(),
            b.route.path(),
        ))
    });
    Ok(rules)
}

/// The configured settings.
pub fn settings(locations: &Locations) -> Result<Settings, Failure> {
    let (_, config) = ConfigFile::new(locations.config_file()).read()?;
    Ok(config.settings)
}

/// Parses a `proxlet logs` selector, with the CLI's message and hint.
pub fn selector(raw: Option<&str>) -> Result<Option<Selector>, Failure> {
    raw.map(Selector::parse).transpose().map_err(|e| {
        Failure::new(e).hint(
            "A selector is a route (`/api`), a domain (`example.com`) or an upstream port (`8000`).",
        )
    })
}

/// The `limit` most recent stored log records the selector shows, oldest
/// first, as stored JSON lines.
pub fn log_records(
    locations: &Locations,
    selector: Option<&Selector>,
    limit: usize,
) -> Result<Vec<String>, Failure> {
    let dir = files::dir(locations);
    let unreadable =
        |e: std::io::Error| Failure::new(format!("cannot read the logs in {}: {e}", dir.display()));
    let wanted = |raw: &str| {
        serde_json::from_str::<Line>(raw).is_ok_and(|line| show::shown(selector, &line))
    };
    let current = Follower::new(&dir).poll().map_err(unreadable)?;
    read::last_matching(&dir, current, limit, wanted).map_err(unreadable)
}

/// Checks whether the front server already serves the rule's host.
fn check_front_conflict(
    locations: &Locations,
    route: &Route,
    raw_target: &str,
    policy: &FrontPolicy,
) -> Result<(), Failure> {
    let Some(host) = route.host() else {
        return Ok(());
    };
    let (_, config) = ConfigFile::new(locations.config_file()).read()?;
    let Some((front, cfg)) = attach::front_config(&config.settings) else {
        return Ok(());
    };
    Conflict {
        front,
        cfg: &cfg,
        port: config.settings.http_port,
        route,
        raw_target,
    }
    .decide(host, policy)
}

struct Conflict<'a> {
    front: &'a str,
    cfg: &'a FrontConfig,
    port: u16,
    route: &'a Route,
    raw_target: &'a str,
}

impl Conflict<'_> {
    fn decide(&self, host: &str, policy: &FrontPolicy) -> Result<(), Failure> {
        let Some(claim) = self.cfg.claims(host, self.port) else {
            return Ok(());
        };
        let front = self.front;
        let serves = format!(
            "{front} serves {host} ({}); requests for it will not reach proxlet",
            claim.at
        );
        match policy {
            FrontPolicy::Allow(true) => Ok(()),
            FrontPolicy::Allow(false) => Err(Failure::new(serves).hint(
                "To add the rule anyway, call add_rule again with allow_front_conflict set to true.",
            )),
            FrontPolicy::Ask { yes, confirm } => {
                output::warn(serves);
                if *yes {
                    return Ok(());
                }
                match confirm("Add the rule anyway?") {
                    Some(true) => Ok(()),
                    Some(false) => Err(Failure::new("nothing was added")),
                    None => Err(Failure::new(format!(
                        "not adding a rule for a host {front} serves without confirmation"
                    ))
                    .hint(format!(
                        "To add it anyway: proxlet add {} {} --yes",
                        self.route, self.raw_target
                    ))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shadowing_config() -> FrontConfig {
        use proxlet_front::config::{Listen, ServerBlock, ServerName};
        let at = proxlet_front::Location {
            file: "/etc/nginx/sites-enabled/site".into(),
            line: 3,
        };
        FrontConfig {
            servers: vec![ServerBlock {
                listens: vec![Listen {
                    port: 80,
                    ipv6: false,
                    default_flag: None,
                    at: at.clone(),
                }],
                names: vec![ServerName {
                    name: "site.example".into(),
                    at: at.clone(),
                }],
                at,
                managed: false,
            }],
            ..FrontConfig::default()
        }
    }

    fn ask(yes: bool, confirm: &dyn Fn(&str) -> Option<bool>) -> FrontPolicy<'_> {
        FrontPolicy::Ask { yes, confirm }
    }

    fn conflict<'a>(cfg: &'a FrontConfig, route: &'a Route) -> Conflict<'a> {
        Conflict {
            front: "nginx",
            cfg,
            port: 80,
            route,
            raw_target: "3000",
        }
    }

    #[test]
    fn conflict_answers_in_the_cli() {
        let cfg = shadowing_config();
        let route: Route = "site.example".parse().unwrap();
        let c = conflict(&cfg, &route);
        assert!(
            c.decide("site.example", &ask(false, &|_| Some(true)))
                .is_ok()
        );
        let declined = c
            .decide("site.example", &ask(false, &|_| Some(false)))
            .unwrap_err();
        assert_eq!(declined.message, "nothing was added");
        let no_terminal = c
            .decide("site.example", &ask(false, &|_| None))
            .unwrap_err();
        assert!(
            no_terminal.hints[0].contains("proxlet add site.example 3000 --yes"),
            "{:?}",
            no_terminal.hints
        );
        assert!(
            c.decide(
                "site.example",
                &ask(true, &|_| panic!("asked despite --yes"))
            )
            .is_ok()
        );
        // Hosts nginx does not serve are never asked about.
        assert!(
            c.decide(
                "other.example",
                &ask(false, &|_| panic!("asked for a free host"))
            )
            .is_ok()
        );
    }

    #[test]
    fn conflict_answers_for_mcp() {
        let cfg = shadowing_config();
        let route: Route = "site.example".parse().unwrap();
        let c = conflict(&cfg, &route);
        let refused = c
            .decide("site.example", &FrontPolicy::Allow(false))
            .unwrap_err();
        assert!(
            refused.message.starts_with("nginx serves site.example"),
            "{}",
            refused.message
        );
        assert!(refused.hints[0].contains("allow_front_conflict"));
        assert!(c.decide("site.example", &FrontPolicy::Allow(true)).is_ok());
        assert!(
            c.decide("other.example", &FrontPolicy::Allow(false))
                .is_ok()
        );
    }
}
