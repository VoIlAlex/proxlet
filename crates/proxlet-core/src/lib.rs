//! Rule model, request matching and configuration for proxlet.
//!
//! This crate has no networking runtime: it defines what a rule is, how a
//! request is matched against the rule table, and how the configuration file is
//! read, validated and written atomically.

pub mod config;
pub mod paths;
pub mod port;
pub mod route;
pub mod rule;
pub mod settings;
pub mod table;
pub mod target;

pub use config::{ChangeError, Config, ConfigDoc, ConfigError, ConfigFile};
pub use paths::Locations;
pub use route::{Route, RouteError};
pub use rule::{Change, Outcome, Rule};
pub use settings::{SETTINGS, SettingDef, SettingError, Settings, Value};
pub use table::RuleTable;
pub use target::{Scheme, Target, TargetError, Upstream};
