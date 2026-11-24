//! The `proxlet.toml` configuration file: rules and settings.
//!
//! The file is edited in place with `toml_edit`, so comments and ordering the
//! user wrote by hand survive changes made from the CLI. Writes are atomic
//! (temp file + rename) and serialised with an advisory lock.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::route::Route;
use crate::rule::{Change, Outcome, Rule};
use crate::settings::{self, Kind, SETTINGS, Settings, Value};
use crate::target::Target;

/// Why the configuration file could not be read, understood or written.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading or writing the file failed.
    #[error("{}: {message}", path.display())]
    Io {
        /// The file involved.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// The file is not valid TOML.
    #[error("{}:{line}:{column}: {message}", path.display())]
    Syntax {
        /// The file involved.
        path: PathBuf,
        /// 1-based line of the error.
        line: usize,
        /// 1-based column of the error.
        column: usize,
        /// The parser's message.
        message: String,
    },
    /// The file is valid TOML but not a valid proxlet configuration.
    #[error("{}: {message}", path.display())]
    Invalid {
        /// The file involved.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
}

/// Why a change could not be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeError {
    /// `rm` named a route that has no rule.
    #[error("there is no rule for `{0}`; run `proxlet ls` to see the rules")]
    NoSuchRule(Route),
    /// The new value conflicts with another setting.
    #[error(transparent)]
    Setting(#[from] crate::settings::SettingError),
}

/// The configuration: settings and rules, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Current settings.
    pub settings: Settings,
    /// Rules in the order they appear in the file.
    pub rules: Vec<Rule>,
}

impl Config {
    /// The rule with exactly this route, if any.
    #[must_use]
    pub fn rule(&self, route: &Route) -> Option<&Rule> {
        self.rules.iter().find(|r| &r.route == route)
    }
}

/// A parsed configuration document that can be edited without losing formatting.
#[derive(Debug, Clone, Default)]
pub struct ConfigDoc {
    path: PathBuf,
    doc: DocumentMut,
}

impl ConfigDoc {
    /// Parses configuration text. `path` is only used in error messages.
    pub fn parse(path: &Path, text: &str) -> Result<Self, ConfigError> {
        let doc = text.parse::<DocumentMut>().map_err(|e| {
            let offset = e.span().map_or(0, |s| s.start);
            let before = &text[..offset.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            ConfigError::Syntax {
                path: path.to_owned(),
                line,
                column,
                message: e.message().trim().to_owned(),
            }
        })?;
        Ok(Self {
            path: path.to_owned(),
            doc,
        })
    }

    /// Interprets the document as a configuration.
    pub fn to_config(&self) -> Result<Config, ConfigError> {
        let invalid = |message: String| ConfigError::Invalid {
            path: self.path.clone(),
            message,
        };
        let mut config = Config::default();

        for (key, item) in self.doc.iter() {
            let is_group = SETTINGS.iter().any(|d| group_and_name(d.key).0 == key);
            if key == "rule" {
                continue;
            }
            if !is_group {
                return Err(invalid(format!("unknown section `{key}`")));
            }
            let table = item
                .as_table_like()
                .ok_or_else(|| invalid(format!("`{key}` must be a table")))?;
            for (name, item) in table.iter() {
                let full = format!("{key}.{name}");
                let def = settings::lookup(&full).map_err(|e| invalid(e.to_string()))?;
                let raw = item_to_setting_text(item, def.kind)
                    .ok_or_else(|| invalid(format!("`{full}` has the wrong type")))?;
                let v = Value::parse(def, &raw).map_err(|e| invalid(e.to_string()))?;
                config.settings.set(def, v);
            }
        }

        if let Some(item) = self.doc.get("rule") {
            let rules = item
                .as_array_of_tables()
                .ok_or_else(|| invalid("`rule` must be written as `[[rule]]` sections".into()))?;
            for (i, table) in rules.iter().enumerate() {
                let rule = rule_from_table(table)
                    .map_err(|m| invalid(format!("rule #{} ({m})", i + 1)))?;
                if config.rule(&rule.route).is_some() {
                    return Err(invalid(format!(
                        "the rule for `{}` appears more than once",
                        rule.route
                    )));
                }
                config.rules.push(rule);
            }
        }
        config
            .settings
            .check(None)
            .map_err(|e| invalid(e.to_string()))?;
        Ok(config)
    }

    /// Applies a change to the document.
    ///
    /// # Panics
    ///
    /// Panics if `current` is not `self.to_config()`.
    pub fn apply(&mut self, current: &Config, change: &Change) -> Result<Outcome, ChangeError> {
        match change {
            Change::AddRule(rule) => {
                let rules = self.rules_mut();
                let existing = current.rules.iter().position(|r| r.route == rule.route);
                if let Some(i) = existing {
                    let table = rules.get_mut(i).expect("rule index matches the document");
                    table.remove("proxy");
                    table.remove("files");
                    set_target(table, &rule.target);
                    Ok(Outcome::Updated {
                        previous: current.rules[i].target.clone(),
                    })
                } else {
                    let mut table = Table::new();
                    table.insert("route", value(rule.route.to_string()));
                    set_target(&mut table, &rule.target);
                    rules.push(table);
                    Ok(Outcome::Added)
                }
            }
            Change::RemoveRule(route) => {
                let i = current
                    .rules
                    .iter()
                    .position(|r| &r.route == route)
                    .ok_or_else(|| ChangeError::NoSuchRule(route.clone()))?;
                let rules = self.rules_mut();
                rules.remove(i);
                if rules.is_empty() {
                    self.doc.remove("rule");
                }
                Ok(Outcome::Removed {
                    previous: current.rules[i].target.clone(),
                })
            }
            Change::SetSetting(def, v) => {
                let previous = current.settings.get(def);
                let mut next = current.settings.clone();
                next.set(def, v.clone());
                next.check(Some(def.key))?;
                let (group, name) = group_and_name(def.key);
                let is_default = *v == Settings::default().get(def);
                if is_default {
                    if let Some(table) = self.doc.get_mut(group).and_then(Item::as_table_mut) {
                        table.remove(name);
                        if table.is_empty() {
                            self.doc.remove(group);
                        }
                    }
                } else {
                    let table = self
                        .doc
                        .entry(group)
                        .or_insert_with(|| Item::Table(Table::new()))
                        .as_table_mut()
                        .expect("settings group is a table");
                    table.insert(name, setting_to_item(v));
                }
                let restart_needed = !def.live && previous != *v;
                Ok(Outcome::SettingChanged {
                    previous,
                    restart_needed,
                })
            }
        }
    }

    fn rules_mut(&mut self) -> &mut ArrayOfTables {
        self.doc
            .entry("rule")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .expect("`rule` is an array of tables (checked by to_config)")
    }
}

impl std::fmt::Display for ConfigDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.doc.fmt(f)
    }
}

fn group_and_name(key: &str) -> (&str, &str) {
    key.split_once('.').expect("setting keys are `group.name`")
}

fn item_to_setting_text(item: &Item, kind: Kind) -> Option<String> {
    let v = item.as_value()?;
    match kind {
        Kind::Bool => v
            .as_bool()
            .map(|b| if b { "on" } else { "off" }.to_owned())
            .or_else(|| v.as_str().map(str::to_owned)),
        Kind::Port => v.as_integer().map(|i| i.to_string()),
        Kind::Ip | Kind::Duration | Kind::Size | Kind::AcmeServer => v.as_str().map(str::to_owned),
    }
}

fn setting_to_item(v: &Value) -> Item {
    match v {
        Value::Bool(b) => value(*b),
        Value::Port(p) => value(i64::from(*p)),
        Value::Ip(_) | Value::Duration(_) | Value::Size(_) | Value::AcmeServer(_) => {
            value(v.to_string())
        }
    }
}

fn set_target(table: &mut Table, target: &Target) {
    match target {
        Target::Upstream(u) => table.insert("proxy", value(u.url())),
        Target::Directory(p) => table.insert("files", value(p.display().to_string())),
    };
}

fn rule_from_table(table: &Table) -> Result<Rule, String> {
    let route_text = table
        .get("route")
        .and_then(Item::as_str)
        .ok_or("missing `route`")?;
    let route: Route = route_text
        .parse()
        .map_err(|e| format!("route `{route_text}`: {e}"))?;
    let proxy = table.get("proxy").and_then(Item::as_str);
    let files = table.get("files").and_then(Item::as_str);
    let target = match (proxy, files) {
        (Some(url), None) => {
            Target::from_stored_proxy(url).map_err(|e| format!("route `{route}`: {e}"))?
        }
        (None, Some(dir)) if Path::new(dir).is_absolute() => Target::Directory(dir.into()),
        (None, Some(dir)) => {
            return Err(format!(
                "route `{route}`: `files` must be an absolute path, got `{dir}`"
            ));
        }
        (None, None) => return Err(format!("route `{route}`: needs `proxy` or `files`")),
        (Some(_), Some(_)) => {
            return Err(format!(
                "route `{route}`: has both `proxy` and `files`; keep one"
            ));
        }
    };
    for (key, _) in table {
        if !matches!(key, "route" | "proxy" | "files") {
            return Err(format!("route `{route}`: unknown key `{key}`"));
        }
    }
    Ok(Rule { route, target })
}

/// The configuration file on disk.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    path: PathBuf,
}

impl ConfigFile {
    /// The file at `path`. Nothing is read until asked.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the file lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads and interprets the file. A missing file is an empty configuration.
    pub fn read(&self) -> Result<(ConfigDoc, Config), ConfigError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(self.io_error(&e)),
        };
        let doc = ConfigDoc::parse(&self.path, &text)?;
        let config = doc.to_config()?;
        Ok((doc, config))
    }

    /// Reads the file, lets `edit` change the document, and writes it back if
    /// `edit` succeeds. The whole sequence holds an exclusive lock, so
    /// concurrent edits never interleave. A failing `edit` leaves the file
    /// untouched.
    pub fn transact<T, E>(
        &self,
        edit: impl FnOnce(&mut ConfigDoc, &Config) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<ConfigError>,
    {
        let _lock = self.lock()?;
        let (mut doc, config) = self.read()?;
        let result = edit(&mut doc, &config)?;
        self.write(&doc)?;
        Ok(result)
    }

    fn lock(&self) -> Result<File, ConfigError> {
        let lock_path = self.path.with_extension("toml.lock");
        self.ensure_dir()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| self.io_error(&e))?;
        file.lock().map_err(|e| self.io_error(&e))?;
        Ok(file)
    }

    fn ensure_dir(&self) -> Result<(), ConfigError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| self.io_error(&e))?;
        }
        Ok(())
    }

    fn write(&self, doc: &ConfigDoc) -> Result<(), ConfigError> {
        self.ensure_dir()?;
        let tmp = self
            .path
            .with_extension(format!("toml.tmp-{}", std::process::id()));
        let result = (|| {
            let mut f = File::create(&tmp)?;
            f.write_all(doc.to_string().as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(self.io_error(&e));
        }
        Ok(())
    }

    fn io_error(&self, e: &std::io::Error) -> ConfigError {
        ConfigError::Io {
            path: self.path.clone(),
            message: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(s: &str) -> Route {
        s.parse().unwrap()
    }

    fn add(r: &str, t: &str) -> Change {
        Change::AddRule(Rule {
            route: route(r),
            target: Target::parse(t, Path::new("/srv")).unwrap(),
        })
    }

    fn apply(file: &ConfigFile, change: &Change) -> Result<Outcome, Box<dyn std::error::Error>> {
        file.transact(|doc, cfg| -> Result<Outcome, Box<dyn std::error::Error>> {
            Ok(doc.apply(cfg, change)?)
        })
    }

    fn temp_file() -> (tempfile::TempDir, ConfigFile) {
        let dir = tempfile::tempdir().unwrap();
        let file = ConfigFile::new(dir.path().join("proxlet.toml"));
        (dir, file)
    }

    #[test]
    fn missing_file_is_empty_config() {
        let (_d, file) = temp_file();
        let (_, cfg) = file.read().unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn add_update_remove_round_trip() {
        let (_d, file) = temp_file();
        assert_eq!(apply(&file, &add("/api", "8000")).unwrap(), Outcome::Added);
        assert_eq!(
            apply(&file, &add("example.com/files", "/srv/site")).unwrap(),
            Outcome::Added
        );
        let out = apply(&file, &add("/api", "9000")).unwrap();
        assert_eq!(
            out,
            Outcome::Updated {
                previous: Target::parse("8000", Path::new("/")).unwrap()
            }
        );
        let (_, cfg) = file.read().unwrap();
        assert_eq!(cfg.rules.len(), 2);
        assert_eq!(cfg.rules[0].target.to_string(), "http://127.0.0.1:9000");
        assert_eq!(cfg.rules[1].target.to_string(), "/srv/site");

        apply(&file, &Change::RemoveRule(route("/api"))).unwrap();
        apply(&file, &Change::RemoveRule(route("example.com/files"))).unwrap();
        let (_, cfg) = file.read().unwrap();
        assert_eq!(cfg.rules, Vec::new());
        assert_eq!(fs::read_to_string(file.path()).unwrap().trim(), "");
    }

    #[test]
    fn remove_unknown_rule_fails_and_leaves_file_unchanged() {
        let (_d, file) = temp_file();
        apply(&file, &add("/api", "8000")).unwrap();
        let before = fs::read(file.path()).unwrap();
        let err = apply(&file, &Change::RemoveRule(route("/nope"))).unwrap_err();
        assert!(err.to_string().contains("no rule for `/nope`"));
        assert_eq!(fs::read(file.path()).unwrap(), before);
    }

    #[test]
    fn comments_survive_edits() {
        let (_d, file) = temp_file();
        let text = "# my proxlet\n\n[static]\n# keep listings off on this box\nlisting = false\n\n[[rule]]\n# the api\nroute = \"/api\"\nproxy = \"http://127.0.0.1:8000\"\n";
        fs::write(file.path(), text).unwrap();
        apply(&file, &add("/web", "3000")).unwrap();
        let def = settings::lookup("proxy.timeout").unwrap();
        let v = Value::parse(def, "2m").unwrap();
        apply(&file, &Change::SetSetting(def, v)).unwrap();
        let after = fs::read_to_string(file.path()).unwrap();
        for kept in ["# my proxlet", "# keep listings off", "# the api"] {
            assert!(after.contains(kept), "lost {kept:?} in:\n{after}");
        }
        let (_, cfg) = file.read().unwrap();
        assert!(!cfg.settings.static_listing);
        assert_eq!(cfg.settings.proxy_timeout.as_secs(), 120);
        assert_eq!(cfg.rules.len(), 2);
    }

    #[test]
    fn setting_back_to_default_removes_key() {
        let (_d, file) = temp_file();
        let def = settings::lookup("static.listing").unwrap();
        let out = apply(&file, &Change::SetSetting(def, Value::Bool(false))).unwrap();
        assert_eq!(
            out,
            Outcome::SettingChanged {
                previous: Value::Bool(true),
                restart_needed: false
            }
        );
        apply(&file, &Change::SetSetting(def, Value::Bool(true))).unwrap();
        assert_eq!(fs::read_to_string(file.path()).unwrap().trim(), "");
    }

    #[test]
    fn non_live_setting_needs_restart() {
        let (_d, file) = temp_file();
        let def = settings::lookup("listen.http_port").unwrap();
        let out = apply(&file, &Change::SetSetting(def, Value::Port(8080))).unwrap();
        assert!(matches!(
            out,
            Outcome::SettingChanged {
                restart_needed: true,
                ..
            }
        ));
    }

    #[test]
    fn syntax_error_reports_line_and_file_is_never_overwritten() {
        let (_d, file) = temp_file();
        let text = "[static]\nlisting = true\n\n[[rule]\nroute = \"/api\"\n";
        fs::write(file.path(), text).unwrap();
        match file.read().unwrap_err() {
            ConfigError::Syntax { line, .. } => assert_eq!(line, 4),
            other => panic!("expected syntax error, got {other}"),
        }
        assert!(apply(&file, &add("/x", "1")).is_err());
        assert_eq!(fs::read_to_string(file.path()).unwrap(), text);
    }

    #[test]
    fn invalid_contents_are_reported() {
        let cases = [
            ("[nope]\na = 1\n", "unknown section `nope`"),
            ("[static]\nlistng = true\n", "did you mean `static.listing`"),
            ("[static]\nlisting = 3\n", "wrong type"),
            ("[[rule]]\nroute = \"/a\"\n", "needs `proxy` or `files`"),
            (
                "[[rule]]\nroute = \"/a\"\nfiles = \"rel\"\n",
                "must be an absolute path",
            ),
            (
                "[[rule]]\nroute = \"/a\"\nproxy = \"http://x:1\"\n[[rule]]\nroute = \"/a/\"\nproxy = \"http://x:2\"\n",
                "more than once",
            ),
        ];
        for (text, needle) in cases {
            let err = ConfigDoc::parse(Path::new("p.toml"), text)
                .unwrap()
                .to_config()
                .unwrap_err();
            assert!(err.to_string().contains(needle), "{text:?}: {err}");
        }
    }

    #[test]
    fn concurrent_edits_do_not_lose_rules() {
        let (_d, file) = temp_file();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    apply(&file, &add(&format!("/r{i}"), &format!("{}", 8000 + i))).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(file.read().unwrap().1.rules.len(), 8);
    }
}
