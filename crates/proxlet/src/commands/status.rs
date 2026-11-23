//! `proxlet status`.

use proxlet_core::Locations;

use crate::control;
use crate::output::{BOLD, CmdResult, DIM, Failure, OK};

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
    let rows = [
        ("mode", s.mode.clone()),
        ("listen", format!("http://{}", s.listen)),
        ("run by", run_by),
        ("uptime", uptime(s.uptime_secs)),
        ("rules", s.rules.to_string()),
        ("version", s.version.clone()),
    ];
    for (k, v) in rows {
        anstream::println!("  {DIM}{k:<8}{DIM:#}{v}");
    }
    Ok(())
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
    use super::uptime;

    #[test]
    fn uptime_is_short() {
        assert_eq!(uptime(45), "45s");
        assert_eq!(uptime(125), "2m 5s");
        assert_eq!(uptime(7380), "2h 3m");
        assert_eq!(uptime(90_000), "1d 1h");
    }
}
