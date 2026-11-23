//! TLS certificates for proxlet: which names can get one, where they are
//! stored, and (in later modules) how they are obtained and renewed through
//! ACME.
//!
//! This crate knows nothing about serving: the server reads the stored PEM
//! files and builds its own TLS configuration from them.

pub mod name;
pub mod pem;
pub mod store;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

use std::path::{Path, PathBuf};

pub use name::{NameError, check_public};
pub use pem::Leaf;
pub use store::{Listing, Meta, Store, Stored};

/// Errors from reading, checking or writing certificates.
#[derive(Debug, thiserror::Error)]
pub enum CertError {
    /// A file could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// A certificate or key is malformed or does not fit.
    #[error("{0}")]
    Invalid(String),
}

impl CertError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source,
        }
    }
}

/// The current time, Unix seconds.
///
/// # Panics
///
/// If the system clock is before 1970.
#[must_use]
pub fn now() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// A Unix time as a calendar date, `YYYY-MM-DD` (UTC).
#[must_use]
pub fn date(unix: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .map_or_else(|_| "?".into(), |t| t.date().to_string())
}

/// Whole days from `now` until `until`, rounded down; negative once past.
#[must_use]
pub fn days_left(until: i64, now: i64) -> i64 {
    (until - now).div_euclid(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_795_000_000), "2026-11-18");
        assert_eq!(days_left(10 * 86_400, 0), 10);
        assert_eq!(days_left(10 * 86_400 - 1, 0), 9);
        assert_eq!(days_left(0, 1), -1);
    }
}
