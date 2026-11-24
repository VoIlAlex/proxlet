//! Settings: named, typed values that control how proxlet listens and serves.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

/// Why a setting could not be read or changed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    /// The key is not a known setting.
    #[error("unknown setting `{key}`{}", suggestion.map(|s| format!("; did you mean `{s}`?")).unwrap_or_default())]
    UnknownKey {
        /// The key the user typed.
        key: String,
        /// The closest known key, if any is close enough.
        suggestion: Option<&'static str>,
    },
    /// The value is not valid for the setting.
    #[error("`{value}` is not a valid value for `{key}`; {expected}")]
    InvalidValue {
        /// The setting key.
        key: &'static str,
        /// The rejected value.
        value: String,
        /// What the setting accepts.
        expected: &'static str,
    },
    /// The value is valid on its own but conflicts with another setting.
    #[error("{message}")]
    Conflict {
        /// The setting key.
        key: &'static str,
        /// What conflicts and how to fix it.
        message: String,
    },
}

/// The type of a setting's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `on`/`off` (also `true`/`false`).
    Bool,
    /// A TCP port, 1-65535.
    Port,
    /// An IPv4 or IPv6 address.
    Ip,
    /// A duration such as `500ms`, `5s`, `2m`, `1h` or `7d`.
    Duration,
    /// A size such as `512KB`, `100MB` or `1GB` (powers of 1024).
    Size,
    /// An ACME server: `production`, `staging` or an `https://` directory URL.
    AcmeServer,
}

impl Kind {
    fn expected(self) -> &'static str {
        match self {
            Self::Bool => "the value must be `on` or `off`",
            Self::Port => "the value must be a port from 1 to 65535",
            Self::Ip => "the value must be an IP address such as `0.0.0.0` or `127.0.0.1`",
            Self::Duration => {
                "the value must be a number followed by `ms`, `s`, `m`, `h` or `d`, e.g. `30s`"
            }
            Self::Size => "the value must be a number followed by `KB`, `MB` or `GB`, e.g. `100MB`",
            Self::AcmeServer => {
                "the value must be `production`, `staging` or an `https://` ACME directory URL"
            }
        }
    }

    /// Values that can be offered for completion, if the set is fixed.
    #[must_use]
    pub fn fixed_values(self) -> &'static [&'static str] {
        match self {
            Self::Bool => &["on", "off"],
            Self::AcmeServer => &["production", "staging"],
            _ => &[],
        }
    }
}

/// A known setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingDef {
    /// Dotted key, e.g. `static.listing`.
    pub key: &'static str,
    /// Value type.
    pub kind: Kind,
    /// Whether a running server picks up a change without a restart.
    pub live: bool,
    /// One-line description.
    pub help: &'static str,
}

/// Every setting proxlet knows, in display order.
pub const SETTINGS: &[SettingDef] = &[
    SettingDef {
        key: "listen.address",
        kind: Kind::Ip,
        live: false,
        help: "Address the HTTP listener binds to",
    },
    SettingDef {
        key: "listen.http_port",
        kind: Kind::Port,
        live: false,
        help: "Port the HTTP listener binds to",
    },
    SettingDef {
        key: "listen.https_port",
        kind: Kind::Port,
        live: false,
        help: "Port the HTTPS listener binds to",
    },
    SettingDef {
        key: "static.listing",
        kind: Kind::Bool,
        live: true,
        help: "List directories that have no index.html",
    },
    SettingDef {
        key: "static.dotfiles",
        kind: Kind::Bool,
        live: true,
        help: "Serve and list files whose name starts with a dot",
    },
    SettingDef {
        key: "proxy.connect_timeout",
        kind: Kind::Duration,
        live: true,
        help: "How long to wait for an upstream to accept a connection",
    },
    SettingDef {
        key: "proxy.timeout",
        kind: Kind::Duration,
        live: true,
        help: "How long to wait for an upstream to start responding (then 504)",
    },
    SettingDef {
        key: "attach.enabled",
        kind: Kind::Bool,
        live: true,
        help: "Attach behind nginx when it holds or wants the HTTP port",
    },
    SettingDef {
        key: "attach.port",
        kind: Kind::Port,
        live: false,
        help: "Loopback port nginx forwards to when proxlet is attached",
    },
    SettingDef {
        key: "tls.redirect",
        kind: Kind::Bool,
        live: true,
        help: "Redirect plain HTTP to HTTPS for hosts that have a certificate",
    },
    SettingDef {
        key: "cert.server",
        kind: Kind::AcmeServer,
        live: true,
        help: "ACME server certificates come from: production, staging or a URL",
    },
    SettingDef {
        key: "log.access",
        kind: Kind::Bool,
        live: true,
        help: "Record every request in the log files",
    },
    SettingDef {
        key: "log.max_size",
        kind: Kind::Size,
        live: true,
        help: "Delete the oldest log files beyond this total size (at least 10MB)",
    },
    SettingDef {
        key: "log.max_age",
        kind: Kind::Duration,
        live: true,
        help: "Delete log files older than this",
    },
];

/// Looks up a setting definition, suggesting the closest key on a miss.
pub fn lookup(key: &str) -> Result<&'static SettingDef, SettingError> {
    SETTINGS
        .iter()
        .find(|d| d.key == key)
        .ok_or_else(|| SettingError::UnknownKey {
            key: key.to_owned(),
            suggestion: suggest(key),
        })
}

fn suggest(key: &str) -> Option<&'static str> {
    SETTINGS
        .iter()
        .map(|d| (levenshtein(key, d.key), d.key))
        .filter(|(dist, k)| *dist <= (k.len() / 3).max(2))
        .min_by_key(|(dist, _)| *dist)
        .map(|(_, k)| k)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Where certificates come from (`cert.server`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AcmeServer {
    /// Let's Encrypt.
    Production,
    /// Let's Encrypt's staging environment (untrusted certificates, high limits).
    Staging,
    /// Any ACME directory, by its `https://` URL.
    Url(String),
}

impl AcmeServer {
    /// Let's Encrypt's production directory.
    pub const PRODUCTION_URL: &'static str = "https://acme-v02.api.letsencrypt.org/directory";
    /// Let's Encrypt's staging directory.
    pub const STAGING_URL: &'static str = "https://acme-staging-v02.api.letsencrypt.org/directory";

    /// The ACME directory URL.
    #[must_use]
    pub fn directory_url(&self) -> &str {
        match self {
            Self::Production => Self::PRODUCTION_URL,
            Self::Staging => Self::STAGING_URL,
            Self::Url(u) => u,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "production" => return Some(Self::Production),
            "staging" => return Some(Self::Staging),
            _ => {}
        }
        let rest = s.strip_prefix("https://")?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        (!host.is_empty() && !s.contains(char::is_whitespace)).then(|| Self::Url(s.to_owned()))
    }
}

impl fmt::Display for AcmeServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Production => f.write_str("production"),
            Self::Staging => f.write_str("staging"),
            Self::Url(u) => f.write_str(u),
        }
    }
}

/// A typed setting value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// A port.
    Port(u16),
    /// An IP address.
    Ip(IpAddr),
    /// A duration.
    Duration(Duration),
    /// A size in bytes.
    Size(u64),
    /// An ACME server.
    AcmeServer(AcmeServer),
}

impl Value {
    /// Parses user input for a setting of the given definition.
    pub fn parse(def: &SettingDef, raw: &str) -> Result<Self, SettingError> {
        let invalid = || SettingError::InvalidValue {
            key: def.key,
            value: raw.to_owned(),
            expected: def.kind.expected(),
        };
        let s = raw.trim();
        match def.kind {
            Kind::Bool => match s.to_ascii_lowercase().as_str() {
                "on" | "true" => Ok(Self::Bool(true)),
                "off" | "false" => Ok(Self::Bool(false)),
                _ => Err(invalid()),
            },
            Kind::Port => match s.parse::<u16>() {
                Ok(p) if p > 0 => Ok(Self::Port(p)),
                _ => Err(invalid()),
            },
            Kind::Ip => s.parse().map(Self::Ip).map_err(|_| invalid()),
            Kind::Duration => parse_duration(s).map(Self::Duration).ok_or_else(invalid),
            Kind::Size => parse_size(s).map(Self::Size).ok_or_else(invalid),
            Kind::AcmeServer => AcmeServer::parse(s)
                .map(Self::AcmeServer)
                .ok_or_else(invalid),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(true) => f.write_str("on"),
            Self::Bool(false) => f.write_str("off"),
            Self::Port(p) => write!(f, "{p}"),
            Self::Ip(ip) => write!(f, "{ip}"),
            Self::Duration(d) => f.write_str(&format_duration(*d)),
            Self::Size(n) => f.write_str(&format_size(*n)),
            Self::AcmeServer(s) => s.fmt(f),
        }
    }
}

/// Splits `123unit` into the number and the unit.
fn number_and_unit(s: &str) -> Option<(u64, &str)> {
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = s.split_at(split);
    Some((num.parse().ok()?, unit))
}

const DURATION_UNITS: [(&str, u64); 5] = [
    ("d", 86_400_000),
    ("h", 3_600_000),
    ("m", 60_000),
    ("s", 1000),
    ("ms", 1),
];

fn parse_duration(s: &str) -> Option<Duration> {
    let (n, unit) = number_and_unit(s)?;
    let (_, ms) = DURATION_UNITS.iter().find(|(u, _)| *u == unit)?;
    let d = Duration::from_millis(n.checked_mul(*ms)?);
    (!d.is_zero()).then_some(d)
}

fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    let (unit, per) = DURATION_UNITS
        .iter()
        .find(|(_, per)| ms.is_multiple_of(u128::from(*per)))
        .expect("every duration is a whole number of milliseconds");
    format!("{}{unit}", ms / u128::from(*per))
}

/// One megabyte (1024 × 1024 bytes).
pub const MB: u64 = 1024 * 1024;

const SIZE_UNITS: [(&str, u64); 3] = [("GB", 1024 * MB), ("MB", MB), ("KB", 1024)];

fn parse_size(s: &str) -> Option<u64> {
    let (n, unit) = number_and_unit(s)?;
    let (_, per) = SIZE_UNITS
        .iter()
        .find(|(u, _)| u.eq_ignore_ascii_case(unit))?;
    let bytes = n.checked_mul(*per)?;
    (bytes > 0).then_some(bytes)
}

fn format_size(bytes: u64) -> String {
    SIZE_UNITS
        .iter()
        .find(|(_, per)| bytes.is_multiple_of(*per))
        .map_or_else(
            || format!("{bytes}B"),
            |(unit, per)| format!("{}{unit}", bytes / per),
        )
}

/// The smallest `log.max_size`: one log file.
pub const MIN_LOG_SIZE: u64 = 10 * MB;

/// The current value of every setting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per on/off setting"
)]
pub struct Settings {
    /// `listen.address`
    pub listen_address: IpAddr,
    /// `listen.http_port`
    pub http_port: u16,
    /// `listen.https_port`
    pub https_port: u16,
    /// `static.listing`
    pub static_listing: bool,
    /// `static.dotfiles`
    pub static_dotfiles: bool,
    /// `proxy.connect_timeout`
    pub connect_timeout: Duration,
    /// `proxy.timeout`
    pub proxy_timeout: Duration,
    /// `attach.enabled`
    pub attach_enabled: bool,
    /// `attach.port`
    pub attach_port: u16,
    /// `tls.redirect`
    pub tls_redirect: bool,
    /// `cert.server`
    pub cert_server: AcmeServer,
    /// `log.access`
    pub log_access: bool,
    /// `log.max_size`, in bytes
    pub log_max_size: u64,
    /// `log.max_age`
    pub log_max_age: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            listen_address: IpAddr::from([0, 0, 0, 0]),
            http_port: 80,
            https_port: 443,
            static_listing: true,
            static_dotfiles: false,
            connect_timeout: Duration::from_secs(5),
            proxy_timeout: Duration::from_secs(60),
            attach_enabled: true,
            attach_port: 47080,
            tls_redirect: true,
            cert_server: AcmeServer::Production,
            log_access: true,
            log_max_size: 100 * MB,
            log_max_age: Duration::from_secs(7 * 86_400),
        }
    }
}

impl Settings {
    /// The value of a known setting.
    #[must_use]
    pub fn get(&self, def: &SettingDef) -> Value {
        match def.key {
            "listen.address" => Value::Ip(self.listen_address),
            "listen.http_port" => Value::Port(self.http_port),
            "listen.https_port" => Value::Port(self.https_port),
            "static.listing" => Value::Bool(self.static_listing),
            "static.dotfiles" => Value::Bool(self.static_dotfiles),
            "proxy.connect_timeout" => Value::Duration(self.connect_timeout),
            "proxy.timeout" => Value::Duration(self.proxy_timeout),
            "attach.enabled" => Value::Bool(self.attach_enabled),
            "attach.port" => Value::Port(self.attach_port),
            "tls.redirect" => Value::Bool(self.tls_redirect),
            "cert.server" => Value::AcmeServer(self.cert_server.clone()),
            "log.access" => Value::Bool(self.log_access),
            "log.max_size" => Value::Size(self.log_max_size),
            "log.max_age" => Value::Duration(self.log_max_age),
            other => unreachable!("setting `{other}` has no field"),
        }
    }

    /// Sets a known setting to an already-parsed value of the right kind.
    ///
    /// # Panics
    ///
    /// Panics if the value's kind does not match the setting.
    pub fn set(&mut self, def: &SettingDef, value: Value) {
        match (def.key, value) {
            ("listen.address", Value::Ip(v)) => self.listen_address = v,
            ("listen.http_port", Value::Port(v)) => self.http_port = v,
            ("listen.https_port", Value::Port(v)) => self.https_port = v,
            ("static.listing", Value::Bool(v)) => self.static_listing = v,
            ("static.dotfiles", Value::Bool(v)) => self.static_dotfiles = v,
            ("proxy.connect_timeout", Value::Duration(v)) => self.connect_timeout = v,
            ("proxy.timeout", Value::Duration(v)) => self.proxy_timeout = v,
            ("attach.enabled", Value::Bool(v)) => self.attach_enabled = v,
            ("attach.port", Value::Port(v)) => self.attach_port = v,
            ("tls.redirect", Value::Bool(v)) => self.tls_redirect = v,
            ("cert.server", Value::AcmeServer(v)) => self.cert_server = v,
            ("log.access", Value::Bool(v)) => self.log_access = v,
            ("log.max_size", Value::Size(v)) => self.log_max_size = v,
            ("log.max_age", Value::Duration(v)) => self.log_max_age = v,
            (key, value) => panic!("value {value:?} does not fit setting `{key}`"),
        }
    }

    /// Checks the settings against each other. `changed` is the key that was
    /// just set, so the message can name it.
    ///
    /// # Errors
    ///
    /// When two settings that must differ are the same, or `log.max_size`
    /// is below [`MIN_LOG_SIZE`].
    pub fn check(&self, changed: Option<&'static str>) -> Result<(), SettingError> {
        if self.log_max_size < MIN_LOG_SIZE {
            return Err(SettingError::Conflict {
                key: "log.max_size",
                message: format!(
                    "`log.max_size` must be at least {}, the size of one log file",
                    format_size(MIN_LOG_SIZE)
                ),
            });
        }
        let ports = [
            ("listen.http_port", "HTTP port", self.http_port),
            ("listen.https_port", "HTTPS port", self.https_port),
            ("attach.port", "attach port", self.attach_port),
        ];
        for (i, a) in ports.iter().enumerate() {
            for b in &ports[i + 1..] {
                if a.2 != b.2 {
                    continue;
                }
                // Name the one just changed first.
                let (first, second) = if changed == Some(b.0) { (b, a) } else { (a, b) };
                return Err(SettingError::Conflict {
                    key: first.0,
                    message: format!(
                        "the {} must differ from the {} (`{}` is {}); pick another port",
                        first.1, second.1, second.0, second.2
                    ),
                });
            }
        }
        Ok(())
    }

    /// Whether a setting differs from its default.
    #[must_use]
    pub fn is_changed(&self, def: &SettingDef) -> bool {
        self.get(def) != Self::default().get(def)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(key: &str) -> &'static SettingDef {
        lookup(key).unwrap()
    }

    #[test]
    fn defaults() {
        let s = Settings::default();
        let shown: Vec<String> = SETTINGS.iter().map(|d| s.get(d).to_string()).collect();
        assert_eq!(
            shown,
            [
                "0.0.0.0",
                "80",
                "443",
                "on",
                "off",
                "5s",
                "1m",
                "on",
                "47080",
                "on",
                "production",
                "on",
                "100MB",
                "7d"
            ]
        );
        assert!(SETTINGS.iter().all(|d| !s.is_changed(d)));
    }

    #[test]
    fn every_setting_round_trips_through_get_and_set() {
        let mut s = Settings::default();
        for d in SETTINGS {
            let v = s.get(d);
            s.set(d, v);
        }
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn bool_values() {
        let d = def("static.listing");
        assert_eq!(Value::parse(d, "on").unwrap(), Value::Bool(true));
        assert_eq!(Value::parse(d, "TRUE").unwrap(), Value::Bool(true));
        assert_eq!(Value::parse(d, "off").unwrap(), Value::Bool(false));
        assert_eq!(Value::parse(d, "false").unwrap(), Value::Bool(false));
        let err = Value::parse(d, "maybe").unwrap_err();
        assert!(err.to_string().contains("`on` or `off`"), "{err}");
    }

    #[test]
    fn port_values() {
        let d = def("listen.http_port");
        assert_eq!(Value::parse(d, "8080").unwrap(), Value::Port(8080));
        assert!(Value::parse(d, "0").is_err());
        assert!(Value::parse(d, "70000").is_err());
    }

    #[test]
    fn ip_values() {
        let d = def("listen.address");
        assert!(Value::parse(d, "127.0.0.1").is_ok());
        assert!(Value::parse(d, "::").is_ok());
        assert!(Value::parse(d, "localhost").is_err());
    }

    #[test]
    fn duration_values() {
        let d = def("proxy.timeout");
        assert_eq!(
            Value::parse(d, "2m").unwrap(),
            Value::Duration(Duration::from_secs(120))
        );
        assert_eq!(Value::parse(d, "1500ms").unwrap().to_string(), "1500ms");
        assert_eq!(Value::parse(d, "30s").unwrap().to_string(), "30s");
        assert_eq!(Value::parse(d, "7d").unwrap().to_string(), "7d");
        assert_eq!(Value::parse(d, "168h").unwrap().to_string(), "7d");
        assert_eq!(Value::parse(d, "90m").unwrap().to_string(), "90m");
        assert_eq!(Value::parse(d, "120m").unwrap().to_string(), "2h");
        assert_eq!(Value::parse(d, "60000ms").unwrap().to_string(), "1m");
        assert_eq!(
            Value::parse(d, "2h").unwrap(),
            Value::Duration(Duration::from_secs(7200))
        );
        for bad in ["", "5", "s", "5w", "0s", "0d", "-1s", "1.5s", "5 h", "5H"] {
            assert!(Value::parse(d, bad).is_err(), "{bad:?}");
        }
        let err = Value::parse(d, "5w").unwrap_err().to_string();
        assert!(err.contains("`h` or `d`"), "{err}");
    }

    #[test]
    fn size_values() {
        let d = def("log.max_size");
        assert_eq!(Value::parse(d, "100MB").unwrap(), Value::Size(100 * MB));
        assert_eq!(Value::parse(d, "1GB").unwrap().to_string(), "1GB");
        assert_eq!(Value::parse(d, "1024MB").unwrap().to_string(), "1GB");
        assert_eq!(Value::parse(d, "1536MB").unwrap().to_string(), "1536MB");
        assert_eq!(Value::parse(d, "512kb").unwrap().to_string(), "512KB");
        assert_eq!(Value::parse(d, "2048KB").unwrap().to_string(), "2MB");
        for bad in ["", "100", "MB", "0MB", "1.5GB", "10TB", "-1MB", "10 MB"] {
            assert!(Value::parse(d, bad).is_err(), "{bad:?}");
        }
        let err = Value::parse(d, "10TB").unwrap_err().to_string();
        assert!(err.contains("`KB`, `MB` or `GB`"), "{err}");
        assert_eq!(format_size(1500), "1500B");
    }

    #[test]
    fn log_size_has_a_minimum() {
        let ok = Settings {
            log_max_size: MIN_LOG_SIZE,
            ..Settings::default()
        };
        assert!(ok.check(None).is_ok());
        let small = Settings {
            log_max_size: MB,
            ..Settings::default()
        };
        let err = small.check(Some("log.max_size")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`log.max_size` must be at least 10MB, the size of one log file"
        );
    }

    #[test]
    fn acme_server_values() {
        let d = def("cert.server");
        assert_eq!(
            Value::parse(d, "staging").unwrap(),
            Value::AcmeServer(AcmeServer::Staging)
        );
        assert_eq!(
            Value::parse(d, "Production").unwrap().to_string(),
            "production"
        );
        let url = "https://localhost:14000/dir";
        assert_eq!(Value::parse(d, url).unwrap().to_string(), url);
        assert_eq!(
            AcmeServer::Url(url.into()).directory_url(),
            url,
            "a custom URL is used as is"
        );
        assert_eq!(AcmeServer::Staging.directory_url(), AcmeServer::STAGING_URL);
        for bad in ["http://acme.local/dir", "https://", "prod", "https://a b"] {
            let err = Value::parse(d, bad).unwrap_err().to_string();
            assert!(
                err.contains("`production`, `staging` or an `https://`"),
                "{bad}: {err}"
            );
        }
        assert_eq!(d.kind.fixed_values(), ["production", "staging"]);
    }

    #[test]
    fn ports_must_differ() {
        assert!(Settings::default().check(None).is_ok());
        let s = Settings {
            https_port: 80,
            ..Settings::default()
        };
        let err = s.check(Some("listen.https_port")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the HTTPS port must differ from the HTTP port (`listen.http_port` is 80); pick another port"
        );
        let s = Settings {
            http_port: 443,
            ..Settings::default()
        };
        let err = s.check(Some("listen.http_port")).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("the HTTP port must differ from the HTTPS port"),
            "{err}"
        );
        let s = Settings {
            attach_port: 443,
            ..Settings::default()
        };
        assert!(s.check(Some("attach.port")).is_err());
    }

    #[test]
    fn unknown_key_suggests_closest() {
        let err = lookup("static.listng").unwrap_err();
        assert_eq!(
            err,
            SettingError::UnknownKey {
                key: "static.listng".into(),
                suggestion: Some("static.listing"),
            }
        );
        assert!(err.to_string().contains("did you mean `static.listing`"));
        assert!(matches!(
            lookup("completely.different.thing"),
            Err(SettingError::UnknownKey {
                suggestion: None,
                ..
            })
        ));
    }
}
