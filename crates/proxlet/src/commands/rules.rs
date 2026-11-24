//! `proxlet add`, `proxlet rm` and `proxlet ls`.

use std::time::Duration;

use proxlet_core::{Locations, Target, port};

use super::note_not_running;
use crate::attach;
use crate::control::WireOutcome;
use crate::ops::{self, FrontPolicy};
use crate::output::{self, BOLD, CmdResult, DIM, WARN};

/// `proxlet add ROUTE TARGET [--yes]`.
pub fn add(locations: &Locations, route: &str, target: &str, yes: bool) -> CmdResult {
    let cwd = std::env::current_dir()?;
    let policy = FrontPolicy::Ask {
        yes,
        confirm: &output::confirm,
    };
    let added = ops::add_rule(locations, route, target, &cwd, &policy)?;
    let (route, target) = (&added.route, &added.target);
    match added.changed.outcome {
        WireOutcome::Updated { previous } => {
            output::success(format!("Updated {route} → {target} (was {previous})"));
        }
        _ => output::success(format!("Added {route} → {target}")),
    }
    if let Target::Upstream(u) = target
        && !port::is_listening(&u.host, u.port, Duration::from_millis(300))
    {
        output::warn(format!(
            "Nothing is listening on {} yet; requests will get 502 until it starts.",
            u.authority()
        ));
    }
    if !added.changed.live {
        note_not_running();
    }
    Ok(())
}

/// `proxlet rm ROUTE`.
pub fn rm(locations: &Locations, route: &str) -> CmdResult {
    let (route, changed) = ops::remove_rule(locations, route)?;
    match changed.outcome {
        WireOutcome::Removed { previous } => {
            output::success(format!("Removed {route} (was → {previous})"));
        }
        _ => output::success(format!("Removed {route}")),
    }
    if !changed.live {
        note_not_running();
    }
    Ok(())
}

/// `proxlet ls`.
pub fn ls(locations: &Locations) -> CmdResult {
    let rules = ops::rules(locations)?;
    if rules.is_empty() {
        anstream::eprintln!("No rules yet. Add one with: {BOLD}proxlet add /api 8000{BOLD:#}");
        return Ok(());
    }
    let settings = ops::settings(locations)?;
    let front = attach::front_config(&settings);
    let rows: Vec<[String; 4]> = rules
        .iter()
        .map(|r| {
            let shadowed = r.route.host().and_then(|host| {
                let (name, cfg) = front.as_ref()?;
                let claim = cfg.claims(host, settings.http_port)?;
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
