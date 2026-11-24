//! The control channel between the CLI and the daemon.
//!
//! Line-delimited JSON over a Unix socket: each request is one JSON object on
//! one line, and so is each reply. The socket file is created with mode 0600,
//! so only the user running the daemon can talk to it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use proxlet_core::{Change, Outcome, Route, Rule, Target, settings};
use serde::{Deserialize, Serialize};

/// A request from the CLI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Liveness check.
    Ping,
    /// Report status.
    Status,
    /// Apply a configuration change.
    Apply {
        /// The change.
        change: WireChange,
    },
    /// Stop the server gracefully.
    Shutdown,
    /// Let go of the public port so a front server can bind it. Replies once
    /// the listener is closed.
    Release,
    /// Try to take the public port back now.
    Reclaim,
    /// Answer a random token for a while, so the CLI can check that a domain
    /// reaches this server.
    Probe,
    /// Get a certificate for a domain.
    Issue {
        /// The domain, already checked by the CLI.
        domain: String,
        /// Whether the user accepted the ACME server's terms.
        #[serde(default)]
        accept_terms: bool,
    },
    /// Renew certificates now, whether due or not (all when empty).
    Renew {
        /// The domains.
        #[serde(default)]
        domains: Vec<String>,
    },
    /// The certificate files changed (e.g. `cert rm`); load them again.
    CertsChanged,
}

/// A certificate the daemon just obtained.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CertReply {
    /// The domain.
    pub domain: String,
    /// End of validity, Unix seconds.
    pub not_after: i64,
}

/// The result of renewing one certificate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenewReply {
    /// The domain.
    pub domain: String,
    /// The new end of validity, when renewal worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
    /// Why it failed, otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A configuration change on the wire. Targets are canonical strings: an
/// upstream URL or an absolute directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireChange {
    /// Add or replace a rule.
    AddRule {
        /// Route text.
        route: String,
        /// Canonical target text.
        target: String,
    },
    /// Remove a rule.
    RemoveRule {
        /// Route text.
        route: String,
    },
    /// Change a setting.
    SetSetting {
        /// Setting key.
        key: String,
        /// Value text.
        value: String,
    },
}

impl WireChange {
    /// Encodes a change.
    #[must_use]
    pub fn from_change(change: &Change) -> Self {
        match change {
            Change::AddRule(rule) => Self::AddRule {
                route: rule.route.to_string(),
                target: rule.target.to_string(),
            },
            Change::RemoveRule(route) => Self::RemoveRule {
                route: route.to_string(),
            },
            Change::SetSetting(def, value) => Self::SetSetting {
                key: def.key.to_owned(),
                value: value.to_string(),
            },
        }
    }

    /// Decodes and validates a change received over the socket.
    pub fn into_change(self) -> Result<Change, String> {
        match self {
            Self::AddRule { route, target } => {
                let route: Route = route.parse().map_err(|e| format!("{e}"))?;
                let target = Target::parse(&target, Path::new("/")).map_err(|e| format!("{e}"))?;
                Ok(Change::AddRule(Rule { route, target }))
            }
            Self::RemoveRule { route } => route
                .parse()
                .map(Change::RemoveRule)
                .map_err(|e: proxlet_core::RouteError| e.to_string()),
            Self::SetSetting { key, value } => {
                let def = settings::lookup(&key).map_err(|e| e.to_string())?;
                let value = settings::Value::parse(def, &value).map_err(|e| e.to_string())?;
                Ok(Change::SetSetting(def, value))
            }
        }
    }
}

/// What a change did, on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WireOutcome {
    /// A rule was added.
    Added,
    /// A rule's target was replaced.
    Updated {
        /// Previous target.
        previous: String,
    },
    /// A rule was removed.
    Removed {
        /// Removed target.
        previous: String,
    },
    /// A setting was changed.
    SettingChanged {
        /// Previous value.
        previous: String,
        /// Whether a restart is needed for it to take effect.
        restart_needed: bool,
    },
}

impl From<&Outcome> for WireOutcome {
    fn from(o: &Outcome) -> Self {
        match o {
            Outcome::Added => Self::Added,
            Outcome::Updated { previous } => Self::Updated {
                previous: previous.to_string(),
            },
            Outcome::Removed { previous } => Self::Removed {
                previous: previous.to_string(),
            },
            Outcome::SettingChanged {
                previous,
                restart_needed,
            } => Self::SettingChanged {
                previous: previous.to_string(),
                restart_needed: *restart_needed,
            },
        }
    }
}

/// What `status` reports.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusInfo {
    /// proxlet version.
    pub version: String,
    /// Daemon process id.
    pub pid: u32,
    /// `standalone`, `attached (nginx)` or `waiting for port <n> (<reason>)`.
    pub mode: String,
    /// The public HTTP address (`listen.address:listen.http_port`).
    pub listen: String,
    /// The internal loopback address a front server forwards to.
    #[serde(default)]
    pub internal: String,
    /// `systemd` when run by the service, `foreground` otherwise.
    pub run_by: String,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
    /// Number of rules being served.
    pub rules: usize,
    /// The HTTPS port's mode, like `mode`.
    #[serde(default)]
    pub https_mode: String,
    /// The public HTTPS address (`listen.address:listen.https_port`).
    #[serde(default)]
    pub https_listen: String,
    /// Number of certificates.
    #[serde(default)]
    pub certs: usize,
    /// Certificates expiring soon or failing to renew.
    #[serde(default)]
    pub cert_warnings: Vec<String>,
}

/// A reply from the daemon.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reply {
    /// Whether the request succeeded.
    pub ok: bool,
    /// Error message when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Status, for `status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusInfo>,
    /// Outcome, for `apply`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<WireOutcome>,
    /// For `probe`: the token and the body the server answers it with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<(String, String)>,
    /// For `issue`: the new certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<CertReply>,
    /// For `issue`: the ACME server's terms must be accepted first; the URL,
    /// when it publishes one (empty otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<String>,
    /// For `renew`: one entry per certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewed: Option<Vec<RenewReply>>,
}

impl Reply {
    /// A bare success.
    #[must_use]
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }

    /// A failure with a message.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            ..Self::default()
        }
    }
}

/// A connection to a running daemon.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
}

impl Client {
    /// Connects to the daemon's socket. Fails if no daemon is listening.
    pub fn connect(socket: &Path) -> std::io::Result<Self> {
        Self::connect_with_timeout(socket, Duration::from_secs(30))
    }

    /// Like [`Client::connect`], waiting at most `timeout` for each reply.
    pub fn connect_with_timeout(socket: &Path, timeout: Duration) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    /// Sends a request and waits for its reply.
    pub fn call(&mut self, request: &Request) -> std::io::Result<Reply> {
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.reader.get_mut().write_all(line.as_bytes())?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "proxlet closed the control connection",
            ));
        }
        Ok(serde_json::from_str(&reply)?)
    }
}

/// The status of the daemon listening on `socket`, or `None` if none is.
#[must_use]
pub fn running(socket: &Path) -> Option<StatusInfo> {
    let mut client = Client::connect(socket).ok()?;
    client.call(&Request::Status).ok()?.status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_stable() {
        let req = Request::Apply {
            change: WireChange::AddRule {
                route: "/api".into(),
                target: "http://127.0.0.1:8000".into(),
            },
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"cmd":"apply","change":{"kind":"add_rule","route":"/api","target":"http://127.0.0.1:8000"}}"#
        );
        let reply = Reply {
            outcome: Some(WireOutcome::Added),
            ..Reply::ok()
        };
        assert_eq!(
            serde_json::to_string(&reply).unwrap(),
            r#"{"ok":true,"outcome":{"outcome":"added"}}"#
        );
    }

    #[test]
    fn changes_round_trip() {
        let changes = [
            WireChange::AddRule {
                route: "example.com/api".into(),
                target: "http://10.0.0.5:9000".into(),
            },
            WireChange::AddRule {
                route: "/files".into(),
                target: "/srv/site".into(),
            },
            WireChange::RemoveRule {
                route: "/files".into(),
            },
            WireChange::SetSetting {
                key: "proxy.timeout".into(),
                value: "2m".into(),
            },
        ];
        for wire in changes {
            let change = wire.clone().into_change().unwrap();
            assert_eq!(WireChange::from_change(&change), wire);
        }
    }

    #[test]
    fn invalid_wire_changes_are_rejected() {
        assert!(
            WireChange::SetSetting {
                key: "nope".into(),
                value: "1".into()
            }
            .into_change()
            .is_err()
        );
        assert!(
            WireChange::RemoveRule {
                route: "bad host/x".into()
            }
            .into_change()
            .is_err()
        );
    }
}
