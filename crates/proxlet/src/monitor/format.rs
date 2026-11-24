//! Cell text for the monitor, shared by the live table and the plain
//! snapshot (design decision 7).

use std::fmt::Write as _;

use crate::control::{StatsReply, StatsRow};

/// Column headers, in order.
pub const HEADERS: [&str; 13] = [
    "RULE", "TARGET", "REQ/S", "TOTAL", "4XX", "5XX", "UP-ERR", "P50", "P95", "P99", "OUT/S",
    "ACTIVE", "WS",
];

/// How many leading columns are left-aligned (the rest are numbers).
pub const LEFT_ALIGNED: usize = 2;

/// The label of the "no rule" row.
pub const NO_RULE: &str = "(no rule)";

/// Requests per second with one decimal: `4.2`.
#[must_use]
pub fn rate(rps: f64) -> String {
    format!("{rps:.1}")
}

/// A latency in milliseconds: `-`, `<1ms`, `12ms`, `1.2s`.
#[must_use]
pub fn latency(ms: Option<f64>) -> String {
    match ms {
        None => "-".to_owned(),
        Some(ms) if ms < 0.5 => "<1ms".to_owned(),
        Some(ms) if ms < 999.5 => format!("{ms:.0}ms"),
        Some(ms) => format!("{:.1}s", ms / 1000.0),
    }
}

/// Bytes per second: `0B/s`, `512B/s`, `2.5KB/s`, `20KB/s`, `1.5MB/s`.
#[must_use]
pub fn bytes_per_sec(bps: f64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bps < 1023.5 {
        return format!("{bps:.0}B/s");
    }
    let mut value = bps;
    for (i, unit) in UNITS.iter().enumerate() {
        value /= 1024.0;
        if value < 1023.5 || i == UNITS.len() - 1 {
            return if value < 9.95 {
                format!("{value:.1}{unit}/s")
            } else {
                format!("{value:.0}{unit}/s")
            };
        }
    }
    unreachable!("the last unit always returns")
}

/// The text of every cell of a row.
#[must_use]
pub fn cells(row: &StatsRow) -> [String; 13] {
    [
        row.rule.clone().unwrap_or_else(|| NO_RULE.to_owned()),
        row.target.clone().unwrap_or_else(|| "-".to_owned()),
        rate(row.rps),
        row.total.to_string(),
        row.status_4xx.to_string(),
        row.status_5xx.to_string(),
        row.upstream_errors.to_string(),
        latency(row.p50_ms),
        latency(row.p95_ms),
        latency(row.p99_ms),
        bytes_per_sec(row.out_bps),
        row.in_flight.to_string(),
        row.websockets.to_string(),
    ]
}

/// Uptime as `45s`, `12m 5s`, `3h 2m` or `2d 4h`.
#[must_use]
pub fn uptime(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

/// The plain snapshot: a header line and one aligned line per row, no
/// colour, columns separated by two spaces.
#[must_use]
pub fn plain(stats: &StatsReply) -> String {
    let rows: Vec<[String; 13]> = stats.rows.iter().map(cells).collect();
    let mut widths = HEADERS.map(str::len);
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let mut line = |cells: &[&str]| {
        let mut text = String::new();
        for (i, (cell, w)) in cells.iter().zip(widths).enumerate() {
            if i > 0 {
                text.push_str("  ");
            }
            let _ = if i < LEFT_ALIGNED {
                write!(text, "{cell:<w$}")
            } else {
                write!(text, "{cell:>w$}")
            };
        }
        out.push_str(text.trim_end());
        out.push('\n');
    };
    line(&HEADERS);
    for row in &rows {
        line(&row.each_ref().map(String::as_str));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_latencies_and_sizes() {
        assert_eq!(rate(0.0), "0.0");
        assert_eq!(rate(4.25), "4.2");
        assert_eq!(rate(120.0), "120.0");

        assert_eq!(latency(None), "-");
        assert_eq!(latency(Some(0.2)), "<1ms");
        assert_eq!(latency(Some(12.4)), "12ms");
        assert_eq!(latency(Some(999.0)), "999ms");
        assert_eq!(latency(Some(1234.0)), "1.2s");
        assert_eq!(latency(Some(65_000.0)), "65.0s");

        assert_eq!(bytes_per_sec(0.0), "0B/s");
        assert_eq!(bytes_per_sec(512.0), "512B/s");
        assert_eq!(bytes_per_sec(2560.0), "2.5KB/s");
        assert_eq!(bytes_per_sec(20_480.0), "20KB/s");
        assert_eq!(bytes_per_sec(1.5 * 1024.0 * 1024.0), "1.5MB/s");
        assert_eq!(bytes_per_sec(3.0 * 1024.0 * 1024.0 * 1024.0), "3.0GB/s");
        assert_eq!(bytes_per_sec(5000.0 * 1024.0 * 1024.0 * 1024.0), "5000GB/s");

        assert_eq!(uptime(45), "45s");
        assert_eq!(uptime(725), "12m 5s");
        assert_eq!(uptime(3 * 3600 + 120), "3h 2m");
        assert_eq!(uptime(2 * 86_400 + 4 * 3600), "2d 4h");
    }

    #[test]
    fn plain_table_is_aligned() {
        let stats = StatsReply {
            uptime_secs: 10,
            connections: 1,
            rows: vec![
                StatsRow {
                    rule: Some("/api".into()),
                    target: Some("127.0.0.1:8000".into()),
                    rps: 4.2,
                    total: 512,
                    status_4xx: 3,
                    status_5xx: 1,
                    upstream_errors: 1,
                    p50_ms: Some(12.0),
                    p95_ms: Some(40.0),
                    p99_ms: Some(88.0),
                    out_bps: 20_480.0,
                    in_flight: 1,
                    websockets: 0,
                },
                StatsRow {
                    total: 2,
                    status_4xx: 2,
                    ..StatsRow::default()
                },
            ],
        };
        assert_eq!(
            plain(&stats),
            "\
RULE       TARGET          REQ/S  TOTAL  4XX  5XX  UP-ERR   P50   P95   P99   OUT/S  ACTIVE  WS
/api       127.0.0.1:8000    4.2    512    3    1       1  12ms  40ms  88ms  20KB/s       1   0
(no rule)  -                 0.0      2    2    0       0     -     -     -    0B/s       0   0
"
        );
    }
}
