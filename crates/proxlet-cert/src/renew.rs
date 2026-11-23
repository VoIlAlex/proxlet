//! When to renew (design decision 9): a certificate is due once a third or
//! less of its lifetime is left, and failures back off 1h, 2h, 4h, then 6h.

use std::time::Duration;

use crate::{Meta, Stored};

/// How often the server looks for certificates that are due.
pub const CHECK_EVERY: Duration = Duration::from_secs(12 * 3600);

/// The longest wait between failed attempts.
pub const MAX_BACKOFF_SECS: i64 = 6 * 3600;

/// `proxlet status` warns about certificates with this many days or fewer left.
pub const WARN_DAYS: i64 = 14;

/// When a certificate valid from `not_before` to `not_after` becomes due.
#[must_use]
pub fn due_at(not_before: i64, not_after: i64) -> i64 {
    not_after - (not_after - not_before) / 3
}

/// The wait after `failures` failed attempts in a row (at least one).
#[must_use]
pub fn backoff_secs(failures: u32) -> i64 {
    let exp = failures.saturating_sub(1).min(16);
    (3600_i64 << exp).min(MAX_BACKOFF_SECS)
}

/// Whether `cert` should be renewed now: it is due, and any backoff from an
/// earlier failure has passed.
#[must_use]
pub fn should_renew(cert: &Stored, now: i64) -> bool {
    now >= due_at(cert.leaf.not_before, cert.leaf.not_after)
        && cert.meta.next_attempt.is_none_or(|t| now >= t)
}

/// The renewal state after a failed attempt at `now`.
#[must_use]
pub fn after_failure(meta: &Meta, error: &str, now: i64) -> Meta {
    let failures = meta.failures.saturating_add(1);
    Meta {
        server: meta.server.clone(),
        last_error: Some(error.to_owned()),
        failures,
        next_attempt: Some(now + backoff_secs(failures)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Leaf;

    const DAY: i64 = 86_400;

    fn cert(not_before: i64, days: i64) -> Stored {
        Stored {
            domain: "app.example.com".into(),
            leaf: Leaf {
                names: vec!["app.example.com".into()],
                not_before,
                not_after: not_before + days * DAY,
                issuer: "R11".into(),
            },
            meta: Meta::default(),
            path: "/x.pem".into(),
        }
    }

    #[test]
    fn a_90_day_certificate_is_due_with_30_days_left() {
        let c = cert(0, 90);
        assert!(!should_renew(&c, 60 * DAY - 1));
        assert!(should_renew(&c, 60 * DAY));
        assert!(should_renew(&c, 61 * DAY), "29 days left");
    }

    #[test]
    fn a_45_day_certificate_is_due_with_15_days_left() {
        let c = cert(0, 45);
        assert!(!should_renew(&c, 29 * DAY));
        assert!(should_renew(&c, 30 * DAY));
    }

    #[test]
    fn failures_back_off_up_to_six_hours() {
        let hours: Vec<i64> = (1..=6).map(|n| backoff_secs(n) / 3600).collect();
        assert_eq!(hours, [1, 2, 4, 6, 6, 6]);
        assert_eq!(backoff_secs(u32::MAX), MAX_BACKOFF_SECS);

        let mut c = cert(0, 90);
        let now = 70 * DAY;
        c.meta = after_failure(&c.meta, "connection refused", now);
        assert_eq!(c.meta.failures, 1);
        assert_eq!(c.meta.last_error.as_deref(), Some("connection refused"));
        assert!(!should_renew(&c, now + 3599), "waits out the backoff");
        assert!(should_renew(&c, now + 3600));
        c.meta = after_failure(&c.meta, "again", now);
        assert_eq!(c.meta.next_attempt, Some(now + 2 * 3600));
    }
}
