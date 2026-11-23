//! `proxlet add`, `proxlet rm` and `proxlet ls`.

use std::time::Duration;

use proxlet_core::{Change, ConfigFile, Locations, Route, Rule, Target, port};
use proxlet_front::FrontConfig;

use super::{note_not_running, submit};
use crate::attach;
use crate::control::WireOutcome;
use crate::output::{self, BOLD, CmdResult, DIM, Failure, WARN};

/// `proxlet add ROUTE TARGET [--yes]`.
pub fn add(locations: &Locations, route: &str, target: &str, yes: bool) -> CmdResult {
    let raw_target = target;
    let route: Route = route.parse()?;
    let cwd = std::env::current_dir()?;
    let target = Target::parse(target, &cwd)?.validate()?;
    check_front_conflict(locations, &route, raw_target, yes)?;
    let submitted = submit(
        locations,
        &Change::AddRule(Rule {
            route: route.clone(),
            target: target.clone(),
        }),
    )?;
    match submitted.outcome {
        WireOutcome::Updated { previous } => {
            output::success(format!("Updated {route} → {target} (was {previous})"));
        }
        _ => output::success(format!("Added {route} → {target}")),
    }
    if let Target::Upstream(u) = &target
        && !port::is_listening(&u.host, u.port, Duration::from_millis(300))
    {
        output::warn(format!(
            "Nothing is listening on {} yet; requests will get 502 until it starts.",
            u.authority()
        ));
    }
    if !submitted.live {
        note_not_running();
    }
    Ok(())
}

/// Warns when the front server already serves the rule's host, and asks.
fn check_front_conflict(
    locations: &Locations,
    route: &Route,
    raw_target: &str,
    yes: bool,
) -> CmdResult {
    let Some(host) = route.host() else {
        return Ok(());
    };
    let (_, config) = ConfigFile::new(locations.config_file()).read()?;
    let Some((front, cfg)) = attach::front_config(&config.settings) else {
        return Ok(());
    };
    let port = config.settings.http_port;
    let conflict = Conflict {
        front,
        cfg: &cfg,
        port,
        route,
        raw_target,
    };
    conflict.decide(host, yes, &output::confirm)
}

struct Conflict<'a> {
    front: &'a str,
    cfg: &'a FrontConfig,
    port: u16,
    route: &'a Route,
    raw_target: &'a str,
}

impl Conflict<'_> {
    fn decide(&self, host: &str, yes: bool, confirm: &dyn Fn(&str) -> Option<bool>) -> CmdResult {
        let Some(claim) = self.cfg.claims(host, self.port) else {
            return Ok(());
        };
        let front = self.front;
        output::warn(format!(
            "{front} serves {host} ({}); requests for it will not reach proxlet",
            claim.at
        ));
        if yes {
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

/// `proxlet rm ROUTE`.
pub fn rm(locations: &Locations, route: &str) -> CmdResult {
    let route: Route = route.parse()?;
    let submitted = submit(locations, &Change::RemoveRule(route.clone()))?;
    match submitted.outcome {
        WireOutcome::Removed { previous } => {
            output::success(format!("Removed {route} (was → {previous})"));
        }
        _ => output::success(format!("Removed {route}")),
    }
    if !submitted.live {
        note_not_running();
    }
    Ok(())
}

/// `proxlet ls`.
pub fn ls(locations: &Locations) -> CmdResult {
    let (_, config) = ConfigFile::new(locations.config_file()).read()?;
    if config.rules.is_empty() {
        anstream::eprintln!("No rules yet. Add one with: {BOLD}proxlet add /api 8000{BOLD:#}");
        return Ok(());
    }
    let mut rules = config.rules;
    rules.sort_by(|a, b| {
        (a.route.host().is_some(), a.route.host(), a.route.path()).cmp(&(
            b.route.host().is_some(),
            b.route.host(),
            b.route.path(),
        ))
    });
    let front = attach::front_config(&config.settings);
    let rows: Vec<[String; 4]> = rules
        .iter()
        .map(|r| {
            let shadowed = r.route.host().and_then(|host| {
                let (name, cfg) = front.as_ref()?;
                let claim = cfg.claims(host, config.settings.http_port)?;
                Some(format!("served by {name} ({})", claim.at))
            });
            [
                r.route.to_string(),
                r.target.kind().to_owned(),
                r.target.to_string(),
                shadowed.unwrap_or_default(),
            ]
        })
        .collect();

    if output::stdout_is_terminal() {
        anstream::print!("{}", table(&rows));
    } else {
        print!("{}", plain(&rows));
    }
    Ok(())
}

/// One rule per line, tab-separated, no decoration. The fourth field says
/// when a front server takes the rule's requests, and is empty otherwise.
fn plain(rows: &[[String; 4]]) -> String {
    use std::fmt::Write as _;
    rows.iter()
        .fold(String::new(), |mut out, [route, kind, target, note]| {
            let _ = writeln!(out, "{route}\t{kind}\t{target}\t{note}");
            out
        })
}

/// An aligned table with a dimmed header and bold routes. Notes are shown
/// in yellow, in a column that appears only when some rule has one.
fn table(rows: &[[String; 4]]) -> String {
    use std::fmt::Write as _;
    let width = |i: usize, header: &str| {
        rows.iter()
            .map(|r| r[i].chars().count())
            .chain([header.len()])
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(0, "ROUTE"), width(1, "KIND"));
    let notes = rows.iter().any(|r| !r[3].is_empty());
    let mut out = if notes {
        let w2 = width(2, "TARGET");
        format!(
            "{DIM}{:<w0$}  {:<w1$}  {:<w2$}  NOTE{DIM:#}\n",
            "ROUTE", "KIND", "TARGET"
        )
    } else {
        format!("{DIM}{:<w0$}  {:<w1$}  TARGET{DIM:#}\n", "ROUTE", "KIND")
    };
    let w2 = width(2, "TARGET");
    for [route, kind, target, note] in rows {
        if note.is_empty() {
            let _ = writeln!(out, "{BOLD}{route:<w0$}{BOLD:#}  {kind:<w1$}  {target}");
        } else {
            let _ = writeln!(
                out,
                "{BOLD}{route:<w0$}{BOLD:#}  {kind:<w1$}  {target:<w2$}  {WARN}{note}{WARN:#}"
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<[String; 4]> {
        [
            ["/api", "proxy", "http://127.0.0.1:8000", ""],
            ["example.com/files", "files", "/home/me/site", ""],
        ]
        .iter()
        .map(|r| r.map(str::to_owned))
        .collect()
    }

    #[test]
    fn table_is_aligned() {
        let text = anstream::adapter::strip_str(&table(&rows())).to_string();
        assert_eq!(
            text,
            "ROUTE              KIND   TARGET\n\
             /api               proxy  http://127.0.0.1:8000\n\
             example.com/files  files  /home/me/site\n"
        );
    }

    #[test]
    fn plain_is_tab_separated() {
        assert_eq!(
            plain(&rows()),
            "/api\tproxy\thttp://127.0.0.1:8000\t\nexample.com/files\tfiles\t/home/me/site\t\n"
        );
    }

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

    #[test]
    fn conflict_answers() {
        let cfg = shadowing_config();
        let route: Route = "site.example".parse().unwrap();
        let c = Conflict {
            front: "nginx",
            cfg: &cfg,
            port: 80,
            route: &route,
            raw_target: "3000",
        };
        assert!(c.decide("site.example", false, &|_| Some(true)).is_ok());
        let declined = c
            .decide("site.example", false, &|_| Some(false))
            .unwrap_err();
        assert_eq!(declined.message, "nothing was added");
        let no_terminal = c.decide("site.example", false, &|_| None).unwrap_err();
        assert!(
            no_terminal.hints[0].contains("proxlet add site.example 3000 --yes"),
            "{:?}",
            no_terminal.hints
        );
        assert!(
            c.decide("site.example", true, &|_| panic!("asked despite --yes"))
                .is_ok()
        );
        // Hosts nginx does not serve are never asked about.
        assert!(
            c.decide("other.example", false, &|_| panic!("asked for a free host"))
                .is_ok()
        );
    }

    #[test]
    fn table_shows_notes_only_when_there_are_some() {
        let mut rows = rows();
        rows[1][3] = "served by nginx (/etc/nginx/sites-enabled/site:3)".into();
        let text = anstream::adapter::strip_str(&table(&rows)).to_string();
        assert_eq!(
            text,
            "ROUTE              KIND   TARGET                 NOTE\n\
             /api               proxy  http://127.0.0.1:8000\n\
             example.com/files  files  /home/me/site          served by nginx (/etc/nginx/sites-enabled/site:3)\n"
        );
    }
}
