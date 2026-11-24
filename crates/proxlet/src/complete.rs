//! Dynamic shell completion: rule routes, setting keys and setting values.
//!
//! Completers read `proxlet.toml` directly rather than asking the daemon, so
//! they answer in milliseconds and work while proxlet is stopped.

use std::ffi::{OsStr, OsString};

use clap_complete::engine::CompletionCandidate;
use proxlet_core::{ConfigFile, Locations, SETTINGS, settings};

fn current_config() -> Option<proxlet_core::Config> {
    ConfigFile::new(Locations::resolve().config_file())
        .read()
        .ok()
        .map(|(_, c)| c)
}

/// Routes of existing rules, with their targets as help text.
#[must_use]
pub fn rule_routes(current: &OsStr) -> Vec<CompletionCandidate> {
    let current = current.to_string_lossy();
    let Some(config) = current_config() else {
        return Vec::new();
    };
    config
        .rules
        .iter()
        .map(|r| (r.route.to_string(), r.target.to_string()))
        .filter(|(route, _)| route.starts_with(current.as_ref()))
        .map(|(route, target)| CompletionCandidate::new(route).help(Some(target.into())))
        .collect()
}

/// Hosts of existing host rules, for `proxlet cert`.
#[must_use]
pub fn rule_hosts(current: &OsStr) -> Vec<CompletionCandidate> {
    let current = current.to_string_lossy();
    let Some(config) = current_config() else {
        return Vec::new();
    };
    let mut hosts: Vec<String> = config
        .rules
        .iter()
        .filter_map(|r| r.route.host().map(str::to_owned))
        .filter(|h| h.starts_with(current.as_ref()))
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts.into_iter().map(CompletionCandidate::new).collect()
}

/// Selectors for `proxlet logs`: rule routes, host-rule domains and
/// upstream ports.
#[must_use]
pub fn log_selectors(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(config) = current_config() else {
        return Vec::new();
    };
    selectors_for(&config, current)
}

/// [`log_selectors`] for a given configuration.
#[must_use]
pub fn selectors_for(config: &proxlet_core::Config, current: &OsStr) -> Vec<CompletionCandidate> {
    let current = current.to_string_lossy();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |value: String, help: String| {
        if value.starts_with(current.as_ref()) && !out.iter().any(|(v, _)| *v == value) {
            out.push((value, help));
        }
    };
    for r in &config.rules {
        // A host's root rule is written `example.com/`, so it is not taken
        // for the domain `example.com`.
        let route = match r.route.host() {
            Some(host) if r.route.path() == "/" => format!("{host}/"),
            _ => r.route.to_string(),
        };
        push(route, format!("requests for this rule ({})", r.target));
    }
    for r in &config.rules {
        if let Some(host) = r.route.host() {
            push(host.to_owned(), "requests for this domain".into());
        }
    }
    for r in &config.rules {
        if let proxlet_core::Target::Upstream(u) = &r.target {
            push(
                u.port.to_string(),
                format!("requests proxied to {}", u.authority()),
            );
        }
    }
    out.into_iter()
        .map(|(v, help)| CompletionCandidate::new(v).help(Some(help.into())))
        .collect()
}

/// Domains that have certificates, with their expiry as help text.
#[must_use]
pub fn cert_domains(current: &OsStr) -> Vec<CompletionCandidate> {
    let current = current.to_string_lossy();
    let store = crate::commands::cert::store(&Locations::resolve());
    let Ok(listing) = store.list() else {
        return Vec::new();
    };
    listing
        .certs
        .into_iter()
        .filter(|c| c.domain.starts_with(current.as_ref()))
        .map(|c| {
            let help = format!("expires {}", proxlet_cert::date(c.leaf.not_after));
            CompletionCandidate::new(c.domain).help(Some(help.into()))
        })
        .collect()
}

/// Setting keys, with their descriptions as help text.
#[must_use]
pub fn setting_keys(current: &OsStr) -> Vec<CompletionCandidate> {
    let current = current.to_string_lossy();
    SETTINGS
        .iter()
        .filter(|d| d.key.starts_with(current.as_ref()))
        .map(|d| CompletionCandidate::new(d.key).help(Some(d.help.into())))
        .collect()
}

/// Accepted values for the setting named earlier on the command line.
#[must_use]
pub fn setting_values(current: &OsStr) -> Vec<CompletionCandidate> {
    values_for(key_from_args(std::env::args_os()).as_deref(), current)
}

/// Values for `key`, or for every fixed-value setting when the key is unknown.
#[must_use]
pub fn values_for(key: Option<&str>, current: &OsStr) -> Vec<CompletionCandidate> {
    let current = current.to_string_lossy();
    let mut values: Vec<&str> = match key.and_then(|k| settings::lookup(k).ok()) {
        Some(def) => def.kind.fixed_values().to_vec(),
        None => SETTINGS
            .iter()
            .flat_map(|d| d.kind.fixed_values())
            .copied()
            .collect(),
    };
    values.dedup();
    values
        .into_iter()
        .filter(|v| v.starts_with(current.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

/// Finds `<key>` in a completion request such as `proxlet -- proxlet config <key> <current>`.
#[must_use]
pub fn key_from_args(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let start = args.iter().position(|a| a == "--").map_or(0, |i| i + 1);
    let words = &args[start..];
    let i = words.iter().position(|a| a == "config")?;
    // `config <key> <current>`: the key is only settled once a word follows it.
    (words.len() > i + 2).then(|| words[i + 1].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(c: &[CompletionCandidate]) -> Vec<String> {
        c.iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn keys_filter_by_prefix() {
        assert_eq!(
            names(&setting_keys(OsStr::new("static."))),
            ["static.listing", "static.dotfiles"]
        );
    }

    #[test]
    fn values_for_bool_setting() {
        assert_eq!(
            names(&values_for(Some("static.listing"), OsStr::new(""))),
            ["on", "off"]
        );
        assert_eq!(
            names(&values_for(Some("static.listing"), OsStr::new("o"))),
            ["on", "off"]
        );
        assert_eq!(
            names(&values_for(Some("proxy.timeout"), OsStr::new(""))),
            Vec::<String>::new()
        );
    }

    #[test]
    fn key_is_read_from_completion_args() {
        let args = |s: &str| s.split(' ').map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            key_from_args(args("proxlet -- proxlet config static.listing ")),
            Some("static.listing".into())
        );
        assert_eq!(key_from_args(args("proxlet -- proxlet config stat")), None);
        assert_eq!(key_from_args(args("proxlet -- proxlet ls")), None);
    }

    #[test]
    fn log_selectors_offer_routes_domains_and_ports() {
        let mut config = proxlet_core::Config::default();
        for (route, target) in [("/api", "8000"), ("example.com/", "3000")] {
            config.rules.push(proxlet_core::Rule {
                route: route.parse().unwrap(),
                target: proxlet_core::Target::parse(target, std::path::Path::new("/")).unwrap(),
            });
        }
        assert_eq!(
            names(&selectors_for(&config, OsStr::new(""))),
            ["/api", "example.com/", "example.com", "8000", "3000"]
        );
        assert_eq!(names(&selectors_for(&config, OsStr::new("8"))), ["8000"]);
    }
}
