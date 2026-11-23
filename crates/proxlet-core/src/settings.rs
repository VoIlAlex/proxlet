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
    /// A duration such as `500ms`, `5s` or `2m`.
    Duration,
}

impl Kind {
    fn expected(self) -> &'static str {
        match self {
            Self::Bool => "the value must be `on` or `off`",
            Self::Port => "the value must be a port from 1 to 65535",
            Self::Ip => "the value must be an IP address such as `0.0.0.0` or `127.0.0.1`",
            Self::Duration => "the value must be a number followed by `ms`, `s` or `m`, e.g. `30s`",
        }
    }

    /// Values that can be offered for completion, if the set is fixed.
    #[must_use]
    pub fn fixed_values(self) -> &'static [&'static str] {
        match self {
            Self::Bool => &["on", "off"],
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

/// A typed setting value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// A port.
    Port(u16),
    /// An IP address.
    Ip(IpAddr),
    /// A duration.
    Duration(Duration),
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
        }
    }
}

fn parse_duration(s: &str) -> Option<Duration> {
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().ok()?;
    let d = match unit {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n.checked_mul(60)?),
        _ => return None,
    };
    (!d.is_zero()).then_some(d)
}

fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

/// The current value of every setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `listen.address`
    pub listen_address: IpAddr,
    /// `listen.http_port`
    pub http_port: u16,
    /// `static.listing`
    pub static_listing: bool,
    /// `static.dotfiles`
    pub static_dotfiles: bool,
    /// `proxy.connect_timeout`
    pub connect_timeout: Duration,
    /// `proxy.timeout`
    pub proxy_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            listen_address: IpAddr::from([0, 0, 0, 0]),
            http_port: 80,
            static_listing: true,
            static_dotfiles: false,
            connect_timeout: Duration::from_secs(5),
            proxy_timeout: Duration::from_secs(60),
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
            "static.listing" => Value::Bool(self.static_listing),
            "static.dotfiles" => Value::Bool(self.static_dotfiles),
            "proxy.connect_timeout" => Value::Duration(self.connect_timeout),
            "proxy.timeout" => Value::Duration(self.proxy_timeout),
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
            ("static.listing", Value::Bool(v)) => self.static_listing = v,
            ("static.dotfiles", Value::Bool(v)) => self.static_dotfiles = v,
            ("proxy.connect_timeout", Value::Duration(v)) => self.connect_timeout = v,
            ("proxy.timeout", Value::Duration(v)) => self.proxy_timeout = v,
            (key, value) => panic!("value {value:?} does not fit setting `{key}`"),
        }
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
        assert_eq!(shown, ["0.0.0.0", "80", "on", "off", "5s", "1m"]);
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
        for bad in ["", "5", "s", "5h", "0s", "-1s", "1.5s"] {
            assert!(Value::parse(d, bad).is_err(), "{bad:?}");
        }
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
