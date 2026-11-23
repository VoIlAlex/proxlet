//! `proxlet add`, `proxlet rm` and `proxlet ls`.

use std::time::Duration;

use proxlet_core::{Change, ConfigFile, Locations, Route, Rule, Target, port};

use super::{note_not_running, submit};
use crate::control::WireOutcome;
use crate::output::{self, BOLD, CmdResult, DIM};

/// `proxlet add ROUTE TARGET`.
pub fn add(locations: &Locations, route: &str, target: &str) -> CmdResult {
    let route: Route = route.parse()?;
    let cwd = std::env::current_dir()?;
    let target = Target::parse(target, &cwd)?.validate()?;
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
    let rows: Vec<[String; 3]> = rules
        .iter()
        .map(|r| {
            [
                r.route.to_string(),
                r.target.kind().to_owned(),
                r.target.to_string(),
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

/// One rule per line, tab-separated, no decoration.
fn plain(rows: &[[String; 3]]) -> String {
    use std::fmt::Write as _;
    rows.iter()
        .fold(String::new(), |mut out, [route, kind, target]| {
            let _ = writeln!(out, "{route}\t{kind}\t{target}");
            out
        })
}

/// An aligned table with a dimmed header and bold routes.
fn table(rows: &[[String; 3]]) -> String {
    use std::fmt::Write as _;
    let width = |i: usize, header: &str| {
        rows.iter()
            .map(|r| r[i].chars().count())
            .chain([header.len()])
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(0, "ROUTE"), width(1, "KIND"));
    let mut out = format!("{DIM}{:<w0$}  {:<w1$}  TARGET{DIM:#}\n", "ROUTE", "KIND");
    for [route, kind, target] in rows {
        let _ = writeln!(out, "{BOLD}{route:<w0$}{BOLD:#}  {kind:<w1$}  {target}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<[String; 3]> {
        [
            ["/api", "proxy", "http://127.0.0.1:8000"],
            ["example.com/files", "files", "/home/me/site"],
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
            "/api\tproxy\thttp://127.0.0.1:8000\nexample.com/files\tfiles\t/home/me/site\n"
        );
    }
}
