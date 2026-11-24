//! The stored record format: one JSON object per line (design decision 1).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use proxlet_server::AccessRecord;
use serde::{Deserialize, Serialize};

/// One stored record. Field order is the order written; absent fields are
/// left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// UTC time, RFC 3339 with milliseconds.
    pub t: String,
    /// `request` or `event`.
    pub kind: String,
    /// Request: the client address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Request: the method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Request: the lowercase host without its port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Request: the path without the query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Request: the response status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Request: body bytes sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Request: duration in milliseconds, to a tenth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<f64>,
    /// Request: the matched rule's route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// Request: the upstream, directory, or other answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Request: why the upstream failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Event: `info`, `warn` or `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Event: the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

/// The `kind` of access records.
pub const REQUEST: &str = "request";
/// The `kind` of event records.
pub const EVENT: &str = "event";

impl Line {
    fn empty(time: SystemTime, kind: &str) -> Self {
        Self {
            t: rfc3339_millis(time),
            kind: kind.to_owned(),
            client: None,
            method: None,
            host: None,
            path: None,
            status: None,
            bytes: None,
            ms: None,
            rule: None,
            target: None,
            error: None,
            level: None,
            msg: None,
        }
    }

    /// The stored form of an access record.
    #[must_use]
    pub fn request(r: AccessRecord) -> Self {
        Self {
            client: Some(r.client),
            method: Some(r.method),
            host: r.host,
            path: Some(r.path),
            status: Some(r.status),
            bytes: Some(r.bytes),
            ms: Some(millis(r.duration)),
            rule: r.rule,
            target: r.target,
            error: r.error,
            ..Self::empty(r.time, REQUEST)
        }
    }

    /// The stored form of a server message.
    #[must_use]
    pub fn event(time: SystemTime, level: &str, msg: String) -> Self {
        Self {
            level: Some(level.to_owned()),
            msg: Some(msg),
            ..Self::empty(time, EVENT)
        }
    }

    /// Whether this is an access record.
    #[must_use]
    pub fn is_request(&self) -> bool {
        self.kind == REQUEST
    }

    /// The JSON line, without the newline.
    ///
    /// # Panics
    ///
    /// Never: every field serializes.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a record always serializes")
    }
}

/// Milliseconds to a tenth.
fn millis(d: Duration) -> f64 {
    (d.as_secs_f64() * 10_000.0).round() / 10.0
}

/// `2026-11-24T01:06:12.040Z`.
#[must_use]
pub fn rfc3339_millis(time: SystemTime) -> String {
    let ms = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let c = Civil::from_unix_millis(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.milli
    )
}

/// Parses the `t` field back into Unix milliseconds.
#[must_use]
pub fn parse_rfc3339_millis(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() != 24 || b[4] != b'-' || b[10] != b'T' || b[19] != b'.' || b[23] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?);
    Some(
        days * 86_400_000
            + num(11..13)? * 3_600_000
            + num(14..16)? * 60_000
            + num(17..19)? * 1000
            + num(20..23)?,
    )
}

/// A calendar date and time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    /// Year.
    pub year: i64,
    /// Month, 1-12.
    pub month: i64,
    /// Day, 1-31.
    pub day: i64,
    /// Hour, 0-23.
    pub hour: i64,
    /// Minute.
    pub minute: i64,
    /// Second.
    pub second: i64,
    /// Millisecond.
    pub milli: i64,
}

impl Civil {
    /// The calendar time of Unix milliseconds (in whatever zone they were
    /// shifted to).
    #[must_use]
    pub fn from_unix_millis(ms: i64) -> Self {
        let days = ms.div_euclid(86_400_000);
        let rest = ms.rem_euclid(86_400_000);
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month,
            day,
            hour: rest / 3_600_000,
            minute: rest / 60_000 % 60,
            second: rest / 1000 % 60,
            milli: rest % 1000,
        }
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    // 2026-11-24T01:06:12.040Z
    const KNOWN: u64 = 1_795_482_372_040;

    #[test]
    fn timestamp_at_a_known_instant() {
        assert_eq!(rfc3339_millis(at(KNOWN)), "2026-11-24T01:06:12.040Z");
        assert_eq!(rfc3339_millis(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_millis(at(951_782_400_000)),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(
            parse_rfc3339_millis("2026-11-24T01:06:12.040Z"),
            Some(i64::try_from(KNOWN).unwrap())
        );
        assert_eq!(parse_rfc3339_millis("2026-11-24 01:06:12Z"), None);
    }

    fn access(path: &str, status: u16, error: Option<&str>) -> AccessRecord {
        AccessRecord {
            time: at(KNOWN),
            client: "203.0.113.7".into(),
            method: "GET".into(),
            host: Some("example.com".into()),
            path: path.into(),
            status,
            bytes: if status == 200 { 512 } else { 96 },
            duration: Duration::from_micros(if status == 200 { 12_449 } else { 812 }),
            rule: Some("/api".into()),
            target: Some("127.0.0.1:8000".into()),
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn request_json() {
        assert_eq!(
            Line::request(access("/api/users", 200, None)).to_json(),
            r#"{"t":"2026-11-24T01:06:12.040Z","kind":"request","client":"203.0.113.7","method":"GET","host":"example.com","path":"/api/users","status":200,"bytes":512,"ms":12.4,"rule":"/api","target":"127.0.0.1:8000"}"#
        );
    }

    #[test]
    fn error_request_json() {
        assert_eq!(
            Line::request(access("/api", 502, Some("connection refused"))).to_json(),
            r#"{"t":"2026-11-24T01:06:12.040Z","kind":"request","client":"203.0.113.7","method":"GET","host":"example.com","path":"/api","status":502,"bytes":96,"ms":0.8,"rule":"/api","target":"127.0.0.1:8000","error":"connection refused"}"#
        );
    }

    #[test]
    fn no_rule_request_leaves_out_rule_and_target() {
        let mut r = access("/x", 404, None);
        r.rule = None;
        r.target = None;
        r.host = None;
        let json = Line::request(r).to_json();
        assert!(!json.contains("rule") && !json.contains("target") && !json.contains("host"));
    }

    #[test]
    fn event_json_round_trips() {
        let line = Line::event(at(KNOWN), "warn", "port 443 is \"busy\"".into());
        let json = line.to_json();
        assert_eq!(
            json,
            r#"{"t":"2026-11-24T01:06:12.040Z","kind":"event","level":"warn","msg":"port 443 is \"busy\""}"#
        );
        let back: Line = serde_json::from_str(&json).unwrap();
        assert_eq!(back, line);
        assert!(!back.is_request());
    }
}
