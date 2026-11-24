//! Which names a public CA can issue for.

use std::net::IpAddr;

/// Suffixes that never resolve publicly, so no public CA can validate them.
pub const RESERVED_SUFFIXES: &[&str] = &["test", "local", "localhost", "internal", "example"];

/// Why a name cannot get a certificate from a public CA.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// An IP address rather than a name.
    #[error("{0} is an IP address; Let's Encrypt issues certificates for domain names here")]
    IpAddress(String),
    /// `localhost` or a name under a reserved suffix.
    #[error(
        "Let's Encrypt cannot issue for `.{suffix}` names because they are not public; \
         use a domain whose DNS points to this machine"
    )]
    NotPublic {
        /// The reserved suffix, e.g. `test`.
        suffix: &'static str,
    },
    /// A wildcard name.
    #[error("wildcard names need the DNS-01 challenge, which proxlet does not support yet")]
    Wildcard,
    /// Not a syntactically valid DNS name.
    #[error("`{name}` is not a valid domain name: {why}")]
    Invalid {
        /// The name as given.
        name: String,
        /// What is wrong with it.
        why: &'static str,
    },
}

/// Checks that `name` is a public DNS name, and returns it in lowercase
/// without a trailing dot. The result is safe to use as a file name.
///
/// # Errors
///
/// When the name is an IP address, a wildcard, not a valid DNS name, or under
/// a reserved suffix.
pub fn check_public(name: &str) -> Result<String, NameError> {
    let invalid = |why| NameError::Invalid {
        name: name.to_owned(),
        why,
    };
    let trimmed = name.trim();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
    let bracketed = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    if bracketed.parse::<IpAddr>().is_ok() {
        return Err(NameError::IpAddress(trimmed.to_owned()));
    }
    if trimmed.starts_with("*.") {
        return Err(NameError::Wildcard);
    }
    if !trimmed.is_ascii() {
        return Err(invalid(
            "it has non-ASCII characters; write it in its `xn--` (punycode) form",
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.is_empty() {
        return Err(invalid("it is empty"));
    }
    if lower.len() > 253 {
        return Err(invalid("it is longer than 253 characters"));
    }
    let labels: Vec<&str> = lower.split('.').collect();
    let last = labels.last().copied().unwrap_or_default();
    if let Some(&suffix) = RESERVED_SUFFIXES.iter().find(|s| **s == last) {
        return Err(NameError::NotPublic { suffix });
    }
    if labels.len() < 2 {
        return Err(invalid(
            "it needs at least two labels, such as `example.com`",
        ));
    }
    for label in &labels {
        if label.is_empty() {
            return Err(invalid("it has an empty label (two dots in a row)"));
        }
        if label.len() > 63 {
            return Err(invalid("a label is longer than 63 characters"));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(invalid(
                "only letters, digits, hyphens and dots are allowed",
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(invalid("a label starts or ends with a hyphen"));
        }
    }
    if last.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("the last label is all digits"));
    }
    Ok(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_names_are_normalized() {
        assert_eq!(check_public("Example.COM").unwrap(), "example.com");
        assert_eq!(
            check_public("a.b-c.example.org.").unwrap(),
            "a.b-c.example.org"
        );
        assert_eq!(
            check_public("xn--bcher-kva.de").unwrap(),
            "xn--bcher-kva.de"
        );
    }

    #[test]
    fn reserved_names() {
        for (name, suffix) in [
            ("myapp.test", "test"),
            ("printer.local", "local"),
            ("localhost", "localhost"),
            ("app.localhost", "localhost"),
            ("db.internal", "internal"),
            ("site.example", "example"),
        ] {
            assert_eq!(
                check_public(name),
                Err(NameError::NotPublic { suffix }),
                "{name}"
            );
        }
        let msg = check_public("myapp.test").unwrap_err().to_string();
        assert!(
            msg.contains("`.test` names because they are not public"),
            "{msg}"
        );
        // `example.com` is a real, public name.
        assert!(check_public("example.com").is_ok());
    }

    #[test]
    fn addresses_and_wildcards() {
        assert!(matches!(
            check_public("192.0.2.1"),
            Err(NameError::IpAddress(_))
        ));
        assert!(matches!(
            check_public("[::1]"),
            Err(NameError::IpAddress(_))
        ));
        assert_eq!(check_public("*.example.com"), Err(NameError::Wildcard));
    }

    #[test]
    fn invalid_names() {
        let long_label = format!("{}.com", "a".repeat(64));
        let long_name = format!("{}com", "a.".repeat(130));
        for bad in [
            "",
            "com",
            "a..b.com",
            "-a.com",
            "a-.com",
            "a_b.com",
            "a/b.com",
            "../etc.com",
            "bücher.de",
            "a.123",
            &long_label,
            &long_name,
        ] {
            assert!(
                matches!(check_public(bad), Err(NameError::Invalid { .. })),
                "{bad:?}: {:?}",
                check_public(bad)
            );
        }
    }
}
