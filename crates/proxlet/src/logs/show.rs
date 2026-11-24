//! Selecting records and printing them (design decision 7).

use std::fmt::Write as _;

use anstyle::Style;
use proxlet_core::Route;

use super::record::{Civil, Line, parse_rfc3339_millis};
use crate::output::{DIM, ERR, OK, WARN};

/// Which access records to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Records whose rule is exactly this route.
    Route(String),
    /// Records proxied to an upstream on this port.
    Port(u16),
    /// Records for this host.
    Domain(String),
}

impl Selector {
    /// Parses `/api`, `example.com/api`, `8000` or `example.com`.
    ///
    /// # Errors
    ///
    /// When the text is neither a valid route, port nor domain.
    pub fn parse(s: &str) -> Result<Self, String> {
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            return match s.parse::<u16>() {
                Ok(p) if p > 0 => Ok(Self::Port(p)),
                _ => Err(format!(
                    "`{s}` is not a valid port; ports go from 1 to 65535"
                )),
            };
        }
        if s.contains('/') {
            let route: Route = s
                .parse()
                .map_err(|e| format!("`{s}` is not a valid route: {e}"))?;
            return Ok(Self::Route(route.to_string()));
        }
        let route: Route = format!("{s}/")
            .parse()
            .map_err(|_| format!("`{s}` is not a route, a port or a domain name"))?;
        match route.host() {
            Some(host) => Ok(Self::Domain(host.to_owned())),
            None => Err(format!("`{s}` is not a route, a port or a domain name")),
        }
    }

    /// Whether a record is selected.
    #[must_use]
    pub fn matches(&self, line: &Line) -> bool {
        if !line.is_request() {
            return false;
        }
        match self {
            Self::Route(r) => line.rule.as_deref() == Some(r.as_str()),
            Self::Domain(d) => line.host.as_deref() == Some(d.as_str()),
            Self::Port(p) => {
                line.rule.is_some()
                    && line
                        .target
                        .as_deref()
                        .and_then(|t| t.rsplit_once(':'))
                        .is_some_and(|(_, port)| port == p.to_string())
            }
        }
    }
}

/// Whether a record is shown for an optional selector: everything without
/// one, only matching access records with one.
#[must_use]
pub fn shown(selector: Option<&Selector>, line: &Line) -> bool {
    selector.is_none_or(|s| s.matches(line))
}

/// Turns stored times into the local wall clock.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    /// Seconds east of UTC.
    pub offset_secs: i64,
    /// Today's local date, as days since the Unix epoch.
    pub today: i64,
}

impl Clock {
    /// The local clock, or UTC when the local offset is unknown.
    #[must_use]
    pub fn local() -> Self {
        let offset_secs =
            time::UtcOffset::current_local_offset().map_or(0, |o| i64::from(o.whole_seconds()));
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
        Self::at(offset_secs, now_ms)
    }

    /// A clock with a fixed offset, at `now_ms` (Unix milliseconds).
    #[must_use]
    pub fn at(offset_secs: i64, now_ms: i64) -> Self {
        Self {
            offset_secs,
            today: (now_ms + offset_secs * 1000).div_euclid(86_400_000),
        }
    }

    /// `01:06:12.040`, with the date in front when it is not today.
    fn show(self, t: &str) -> String {
        let Some(ms) = parse_rfc3339_millis(t) else {
            return t.to_owned();
        };
        let local = ms + self.offset_secs * 1000;
        let c = Civil::from_unix_millis(local);
        let time = format!(
            "{:02}:{:02}:{:02}.{:03}",
            c.hour, c.minute, c.second, c.milli
        );
        if local.div_euclid(86_400_000) == self.today {
            time
        } else {
            format!("{:04}-{:02}-{:02} {time}", c.year, c.month, c.day)
        }
    }
}

fn status_style(status: u16) -> Style {
    match status {
        500.. => ERR,
        400..=499 => WARN,
        _ => OK,
    }
}

fn duration(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{ms:.0}ms")
    } else {
        format!("{:.1}s", ms / 1000.0)
    }
}

fn size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "one decimal is shown")]
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", b / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0)
    }
}

/// One aligned line. Colour codes are written as styles; `anstream` removes
/// them when stdout is not a terminal.
#[must_use]
pub fn format(line: &Line, clock: Clock) -> String {
    let time = clock.show(&line.t);
    let mut out = String::new();
    if line.is_request() {
        let status = line.status.unwrap_or(0);
        let style = status_style(status);
        let place = format!(
            "{}{}",
            line.host.as_deref().unwrap_or(""),
            line.path.as_deref().unwrap_or("")
        );
        let _ = write!(
            out,
            "{time}  {style}{status:>3}{style:#}  {:<6} {place:<28} {:>6} {:>8}",
            line.method.as_deref().unwrap_or("-"),
            duration(line.ms.unwrap_or(0.0)),
            size(line.bytes.unwrap_or(0)),
        );
        if let Some(target) = &line.target {
            let _ = write!(out, "  → {target}");
        }
        if let Some(error) = &line.error {
            let _ = write!(out, "  {ERR}{error}{ERR:#}");
        }
    } else {
        let level = line.level.as_deref().unwrap_or("info");
        let style = match level {
            "error" => ERR,
            "warn" => WARN,
            _ => DIM,
        };
        let shown = level.to_ascii_uppercase();
        let _ = write!(
            out,
            "{time}  {style}{shown:<5}{style:#}       {}",
            line.msg.as_deref().unwrap_or("")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(json: &str) -> Line {
        serde_json::from_str(json).unwrap()
    }

    const OK_LINE: &str = r#"{"t":"2026-11-24T01:06:12.040Z","kind":"request","client":"203.0.113.7","method":"GET","host":"example.com","path":"/api/users","status":200,"bytes":512,"ms":12.4,"rule":"/api","target":"127.0.0.1:8000"}"#;
    const BAD_LINE: &str = r#"{"t":"2026-11-24T01:06:13.501Z","kind":"request","client":"203.0.113.7","method":"GET","host":"example.com","path":"/api","status":502,"bytes":96,"ms":0.8,"rule":"/api","target":"127.0.0.1:8000","error":"connection refused"}"#;
    const EVENT_LINE: &str = r#"{"t":"2026-11-24T01:06:20.002Z","kind":"event","level":"warn","msg":"port 443 is already in use"}"#;

    // 2026-11-24 12:00 UTC.
    const NOON: i64 = 1_795_521_600_000;

    fn plain(s: &str) -> String {
        anstream::adapter::strip_str(s).to_string()
    }

    #[test]
    fn lines_are_aligned() {
        let utc = Clock::at(0, NOON);
        assert_eq!(
            plain(&format(&request(OK_LINE), utc)),
            "01:06:12.040  200  GET    example.com/api/users          12ms    512 B  → 127.0.0.1:8000"
        );
        assert_eq!(
            plain(&format(&request(BAD_LINE), utc)),
            "01:06:13.501  502  GET    example.com/api                 1ms     96 B  → 127.0.0.1:8000  connection refused"
        );
        assert_eq!(
            plain(&format(&request(EVENT_LINE), utc)),
            "01:06:20.002  WARN        port 443 is already in use"
        );
    }

    #[test]
    fn status_is_coloured_in_a_terminal() {
        let s = format(&request(BAD_LINE), Clock::at(0, NOON));
        assert!(s.contains(&format!("{ERR}502{ERR:#}")), "{s:?}");
    }

    #[test]
    fn date_shown_when_not_today_and_local_offset_applied() {
        // UTC+2: 01:06 UTC is 03:06 local on the same day.
        let plus2 = Clock::at(7200, NOON);
        assert!(plain(&format(&request(OK_LINE), plus2)).starts_with("03:06:12.040  200"));
        // Two days later, the date is shown.
        let later = Clock::at(0, NOON + 2 * 86_400_000);
        assert!(
            plain(&format(&request(OK_LINE), later)).starts_with("2026-11-24 01:06:12.040  200")
        );
    }

    #[test]
    fn sizes_and_durations() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(duration(0.4), "0ms");
        assert_eq!(duration(850.0), "850ms");
        assert_eq!(duration(1234.0), "1.2s");
    }

    #[test]
    fn selectors() {
        assert_eq!(Selector::parse("8000"), Ok(Selector::Port(8000)));
        assert_eq!(Selector::parse("/api"), Ok(Selector::Route("/api".into())));
        assert_eq!(
            Selector::parse("Example.com/api"),
            Ok(Selector::Route("example.com/api".into()))
        );
        // A host's root rule, as completion offers it.
        assert_eq!(
            Selector::parse("example.com/"),
            Ok(Selector::Route("example.com".into()))
        );
        assert_eq!(
            Selector::parse("Example.com"),
            Ok(Selector::Domain("example.com".into()))
        );
        for bad in ["70000", "0", "bad host", "-x", "a..b"] {
            assert!(Selector::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn matching() {
        let ok = request(OK_LINE);
        let event = request(EVENT_LINE);
        assert!(Selector::Route("/api".into()).matches(&ok));
        assert!(!Selector::Route("/ap".into()).matches(&ok));
        assert!(Selector::Port(8000).matches(&ok));
        assert!(!Selector::Port(800).matches(&ok));
        assert!(Selector::Domain("example.com".into()).matches(&ok));
        assert!(!Selector::Domain("example.com".into()).matches(&event));
        assert!(shown(None, &event));
        assert!(!shown(Some(&Selector::Port(8000)), &event));
    }
}
