//! Rules and the changes that can be made to the configuration.

use crate::route::Route;
use crate::settings::{SettingDef, Value};
use crate::target::Target;

/// A rule: requests matching `route` are sent to `target`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rule {
    /// Where requests come in.
    pub route: Route,
    /// Where they go.
    pub target: Target,
}

/// One validated change to the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Add a rule, or replace the target of the rule with the same route.
    AddRule(Rule),
    /// Remove the rule with exactly this route.
    RemoveRule(Route),
    /// Change a setting.
    SetSetting(&'static SettingDef, Value),
}

/// What a change did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A new rule was added.
    Added,
    /// An existing rule's target was replaced.
    Updated {
        /// The target before the change.
        previous: Target,
    },
    /// A rule was removed.
    Removed {
        /// The target the removed rule had.
        previous: Target,
    },
    /// A setting was changed (or set to the value it already had).
    SettingChanged {
        /// The value before the change.
        previous: Value,
        /// Whether a running server must restart to pick it up.
        restart_needed: bool,
    },
}
