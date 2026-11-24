//! `proxlet status`.

use proxlet_core::{ConfigFile, Locations};

use crate::attach;
use crate::control;
use crate::output::{self, BOLD, CmdResult, DIM, Failure, OK};

/// Exit code when proxlet is not running (as in LSB `status`).
pub const NOT_RUNNING: u8 = 3;

/// Prints whether proxlet is running and how.
pub fn run(locations: &Locations) -> CmdResult {
    let Some(s) = control::running(&locations.socket()) else {
        anstream::println!("{DIM}○{DIM:#} proxlet is {BOLD}not running{BOLD:#}");
        return Err(Failure::quiet()
            .hint("Start it with: proxlet start   (or in the foreground: proxlet run)")
            .code(NOT_RUNNING));
    };
    let run_by = if s.run_by == "foreground" {
        format!("foreground (pid {})", s.pid)
    } else {
        format!("{} service (pid {})", s.run_by, s.pid)
    };
    anstream::println!("{OK}●{OK:#} proxlet is {BOLD}running{BOLD:#}");
    let mut rows = vec![("mode", s.mode.clone()), ("listen", listen_line(&s))];
    // A server from before HTTPS existed reports no HTTPS mode.
    if !s.https_mode.is_empty() {
        rows.push(("https", https_line(&s)));
    }
    rows.extend([
        ("run by", run_by),
        ("uptime", uptime(s.uptime_secs)),
        ("rules", s.rules.to_string()),
        ("version", s.version.clone()),
    ]);
    for (k, v) in rows {
        anstream::println!("  {DIM}{k:<8}{DIM:#}{v}");
    }
    for w in &s.cert_warnings {
        output::warn(w);
    }
    if !s.cert_warnings.is_empty() {
        output::note("See them with: proxlet cert ls");
    }
    if let Some(front) = s
        .mode
        .strip_prefix("attached (")
        .and_then(|m| m.strip_suffix(')'))
        && !front_forwards(locations)
    {
        output::warn(format!("{front} is not forwarding to proxlet yet"));
        output::note("Set it up with: sudo proxlet start");
    } else if s.mode.starts_with("attached (") && https_not_forwarded(locations) {
        output::warn("HTTPS is not forwarded to proxlet yet");
        output::note("Set it up with: sudo proxlet start");
    }
    Ok(())
}

/// Whether the front server's config has proxlet's block. Unknown counts as yes.
fn front_forwards(locations: &Locations) -> bool {
    let Ok((_, config)) = ConfigFile::new(locations.config_file()).read() else {
        return true;
    };
    attach::front_config(&config.settings).is_none_or(|(_, cfg)| cfg.has_managed_block())
}

/// Whether proxlet attached before it could forward HTTPS (an old ledger)
/// while the front server could do it now.
fn https_not_forwarded(locations: &Locations) -> bool {
    let Ok(Some(ledger)) = proxlet_front::Ledger::load(&locations.state_dir.join(attach::LEDGER))
    else {
        return false;
    };
    if ledger.https_port.is_some() {
        return false;
    }
    let front = attach::system_front();
    front
        .detect()
        .is_some_and(|inst| front.https_support(&inst).is_ok())
}

/// Where requests reach proxlet, given its mode.
fn listen_line(s: &control::StatusInfo) -> String {
    let port = s.listen.rsplit(':').next().unwrap_or("80");
    if let Some(front) = s
        .mode
        .strip_prefix("attached (")
        .and_then(|m| m.strip_suffix(')'))
    {
        return format!("http://{} (behind {front} on port {port})", s.internal);
    }
    if s.mode == "standalone" || s.internal.is_empty() {
        return format!("http://{}", s.listen);
    }
    format!("http://{} (port {port} not available yet)", s.internal)
}

/// The HTTPS port's state: where proxlet serves it, or why it does not.
fn https_line(s: &control::StatusInfo) -> String {
    if s.https_mode == "standalone" {
        let n = s.certs;
        return format!(
            "https://{} ({n} certificate{})",
            s.https_listen,
            if n == 1 { "" } else { "s" }
        );
    }
    s.https_mode.clone()
}

fn uptime(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::{https_line, listen_line, uptime};
    use crate::control::StatusInfo;

    fn info(mode: &str) -> StatusInfo {
        StatusInfo {
            version: "0".into(),
            pid: 1,
            mode: mode.into(),
            listen: "0.0.0.0:80".into(),
            internal: "127.0.0.1:47080".into(),
            run_by: "foreground".into(),
            uptime_secs: 0,
            rules: 0,
            https_mode: "standalone".into(),
            https_listen: "0.0.0.0:443".into(),
            certs: 2,
            cert_warnings: Vec::new(),
        }
    }

    #[test]
    fn https_line_follows_its_mode() {
        assert_eq!(
            https_line(&info("standalone")),
            "https://0.0.0.0:443 (2 certificates)"
        );
        let mut s = info("standalone");
        s.certs = 1;
        assert_eq!(https_line(&s), "https://0.0.0.0:443 (1 certificate)");
        s.https_mode = "attached (nginx)".into();
        assert_eq!(https_line(&s), "attached (nginx)");
        s.https_mode = "waiting for port 443 (held by caddy)".into();
        assert_eq!(https_line(&s), "waiting for port 443 (held by caddy)");
    }

    #[test]
    fn listen_line_follows_the_mode() {
        assert_eq!(listen_line(&info("standalone")), "http://0.0.0.0:80");
        assert_eq!(
            listen_line(&info("attached (nginx)")),
            "http://127.0.0.1:47080 (behind nginx on port 80)"
        );
        assert_eq!(
            listen_line(&info("waiting for port 80 (held by caddy)")),
            "http://127.0.0.1:47080 (port 80 not available yet)"
        );
    }

    #[test]
    fn uptime_is_short() {
        assert_eq!(uptime(45), "45s");
        assert_eq!(uptime(125), "2m 5s");
        assert_eq!(uptime(7380), "2h 3m");
        assert_eq!(uptime(90_000), "1d 1h");
    }
}
