//! The tools `proxlet mcp` offers (design decision 3). Each one runs the same
//! code as its CLI command, through [`crate::ops`].

use std::path::Path;

use proxlet_core::{Locations, SETTINGS, Settings, Target};
use serde_json::{Map, Value, json};

use super::Tools;
use crate::control;
use crate::monitor::Daemon;
use crate::ops::{self, Changed, FrontPolicy};
use crate::output::Failure;

/// Most records `read_logs` returns.
pub const MAX_LOG_RECORDS: u64 = 500;
/// Records `read_logs` returns by default.
pub const DEFAULT_LOG_RECORDS: u64 = 50;

/// The tools, for the proxlet whose files are at `locations`.
#[derive(Debug)]
pub struct ProxletTools {
    locations: Locations,
}

impl ProxletTools {
    /// Tools for this user's proxlet.
    #[must_use]
    pub fn new(locations: Locations) -> Self {
        Self { locations }
    }
}

fn tool(
    name: &str,
    description: &str,
    read_only: bool,
    properties: &Value,
    required: &[&str],
) -> Value {
    let annotations = if read_only {
        json!({"readOnlyHint": true})
    } else {
        json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true})
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        },
        "annotations": annotations
    })
}

const ROUTE: &str = "A route: a path prefix (`/api`), or a host and path (`example.com/api`, \
    `example.com` for a whole host).";

impl Tools for ProxletTools {
    fn list(&self) -> Vec<Value> {
        let none = json!({});
        vec![
            tool(
                "status",
                "Whether proxlet is running and how: mode, listen addresses, uptime, rule and certificate counts.",
                true,
                &none,
                &[],
            ),
            tool(
                "list_rules",
                "Every rule: its route, kind (`proxy` or `files`) and target.",
                true,
                &none,
                &[],
            ),
            tool(
                "get_settings",
                "Every setting with its value, default, whether it applies live, and what it does.",
                true,
                &none,
                &[],
            ),
            tool(
                "read_logs",
                "The most recent stored log records, oldest first: requests (client, method, host, path, status, bytes, duration, rule, upstream, error) and, without a selector, server events.",
                true,
                &json!({
                    "selector": {"type": "string", "description": "Only requests answered by a route (`/api`), for a domain (`example.com`) or proxied to an upstream port (`8000`)."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LOG_RECORDS, "default": DEFAULT_LOG_RECORDS}
                }),
                &[],
            ),
            tool(
                "get_stats",
                "Live statistics per rule, as `proxlet monitor` shows them: requests per second, totals, 4xx/5xx, upstream errors, p50/p95/p99 latency, bytes per second, requests in flight, open WebSockets. Needs proxlet to be running.",
                true,
                &none,
                &[],
            ),
            tool(
                "add_rule",
                "Adds a rule, or points an existing route at a new target. Validated, saved atomically, and applied live when proxlet runs.",
                false,
                &json!({
                    "route": {"type": "string", "description": ROUTE},
                    "target": {"type": "string", "description": "A local port (`8000`), `host:port`, an `http(s)://host:port` URL, or an absolute directory path to serve files from."},
                    "allow_front_conflict": {"type": "boolean", "default": false, "description": "Add the rule even though the front server (nginx) already serves its host, so requests for it will not reach proxlet."}
                }),
                &["route", "target"],
            ),
            tool(
                "remove_rule",
                "Removes the rule for a route.",
                false,
                &json!({"route": {"type": "string", "description": ROUTE}}),
                &["route"],
            ),
            tool(
                "set_setting",
                "Changes a setting (see get_settings for keys and values).",
                false,
                &json!({
                    "key": {"type": "string", "description": "A setting name, e.g. `static.listing`."},
                    "value": {"type": "string", "description": "The new value, as `proxlet config KEY VALUE` takes it (`on`/`off`, numbers, durations like `30s`, sizes like `100MB`)."}
                }),
                &["key", "value"],
            ),
        ]
    }

    fn call(&self, name: &str, args: &Value) -> Option<Result<Value, Failure>> {
        let l = &self.locations;
        Some(match name {
            "status" => Ok(status(l)),
            "list_rules" => list_rules(l),
            "get_settings" => ops::settings(l).map(|s| settings(&s)),
            "read_logs" => read_logs(l, args),
            "get_stats" => get_stats(l),
            "add_rule" => add_rule(l, args),
            "remove_rule" => remove_rule(l, args),
            "set_setting" => set_setting(l, args),
            _ => return None,
        })
    }
}

fn text_arg<'a>(args: &'a Value, tool: &str, key: &str) -> Result<&'a str, Failure> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(Failure::new(format!("{tool}: `{key}` must be a string"))),
        None => Err(Failure::new(format!("{tool} needs `{key}`"))),
    }
}

fn status(l: &Locations) -> Value {
    match control::running(&l.socket()) {
        None => json!({"running": false, "hint": "Start it with: proxlet start"}),
        Some(info) => {
            let mut v = serde_json::to_value(info).unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("running".into(), json!(true));
            }
            v
        }
    }
}

fn list_rules(l: &Locations) -> Result<Value, Failure> {
    let rules = ops::rules(l)?;
    Ok(Value::Array(
        rules
            .iter()
            .map(|r| {
                json!({
                    "route": r.route.to_string(),
                    "kind": r.target.kind(),
                    "target": r.target.to_string()
                })
            })
            .collect(),
    ))
}

fn settings(current: &Settings) -> Value {
    let defaults = Settings::default();
    Value::Array(
        SETTINGS
            .iter()
            .map(|def| {
                json!({
                    "key": def.key,
                    "value": current.get(def).to_string(),
                    "default": defaults.get(def).to_string(),
                    "live": def.live,
                    "help": def.help
                })
            })
            .collect(),
    )
}

fn read_logs(l: &Locations, args: &Value) -> Result<Value, Failure> {
    let selector = match args.get("selector") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return Err(Failure::new("read_logs: `selector` must be a string")),
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LOG_RECORDS,
        Some(v) => match v.as_u64() {
            Some(n) if (1..=MAX_LOG_RECORDS).contains(&n) => n,
            _ => {
                return Err(Failure::new(format!(
                    "read_logs: `limit` must be a whole number from 1 to {MAX_LOG_RECORDS}"
                )));
            }
        },
    };
    let selector = ops::selector(selector)?;
    let lines = ops::log_records(
        l,
        selector.as_ref(),
        usize::try_from(limit).unwrap_or(usize::MAX),
    )?;
    Ok(Value::Array(
        lines
            .iter()
            .filter_map(|raw| serde_json::from_str(raw).ok())
            .collect(),
    ))
}

fn get_stats(l: &Locations) -> Result<Value, Failure> {
    let stats = Daemon::new(l.socket())
        .fetch()
        .map_err(crate::commands::monitor::failure)?;
    serde_json::to_value(stats).map_err(|e| Failure::new(format!("cannot encode statistics: {e}")))
}

/// The result of a change: the outcome's fields, plus `live`.
fn changed(extra: &[(&str, String)], changed: &Changed) -> Value {
    let mut obj = match serde_json::to_value(&changed.outcome) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for (k, v) in extra {
        obj.insert((*k).to_owned(), json!(v));
    }
    obj.insert("live".into(), json!(changed.live));
    if !changed.live {
        obj.insert(
            "note".into(),
            json!("proxlet is not running; this applies when it starts (proxlet start)."),
        );
    }
    Value::Object(obj)
}

fn add_rule(l: &Locations, args: &Value) -> Result<Value, Failure> {
    let route = text_arg(args, "add_rule", "route")?;
    let target = text_arg(args, "add_rule", "target")?;
    let allow = match args.get("allow_front_conflict") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(Failure::new(
                "add_rule: `allow_front_conflict` must be true or false",
            ));
        }
    };
    // The server's working directory is whatever the AI tool chose, so
    // directories must be absolute (design decision 2).
    let root = Path::new("/");
    if matches!(Target::parse(target, root), Ok(Target::Directory(_)))
        && !Path::new(target.trim()).is_absolute()
    {
        return Err(Failure::new(format!(
            "`{target}` is a relative directory; give an absolute path, such as /home/me/site"
        )));
    }
    let added = ops::add_rule(l, route, target, root, &FrontPolicy::Allow(allow))?;
    Ok(changed(
        &[
            ("route", added.route.to_string()),
            ("target", added.target.to_string()),
        ],
        &added.changed,
    ))
}

fn remove_rule(l: &Locations, args: &Value) -> Result<Value, Failure> {
    let route = text_arg(args, "remove_rule", "route")?;
    let (route, c) = ops::remove_rule(l, route)?;
    Ok(changed(&[("route", route.to_string())], &c))
}

fn set_setting(l: &Locations, args: &Value) -> Result<Value, Failure> {
    let key = text_arg(args, "set_setting", "key")?;
    let value = match args.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => if *b { "on" } else { "off" }.to_owned(),
        Some(_) => return Err(Failure::new("set_setting: `value` must be a string")),
        None => return Err(Failure::new("set_setting needs `value`")),
    };
    let (def, value, c) = ops::set_setting(l, key, &value)?;
    Ok(changed(
        &[("key", def.key.to_owned()), ("value", value.to_string())],
        &c,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_tools_with_schemas_and_annotations() {
        let tools = ProxletTools::new(Locations::under(Path::new("/nonexistent")));
        let list = tools.list();
        let names: Vec<&str> = list.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "status",
                "list_rules",
                "get_settings",
                "read_logs",
                "get_stats",
                "add_rule",
                "remove_rule",
                "set_setting"
            ]
        );
        for t in &list {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
            let read_only = t["annotations"]["readOnlyHint"].as_bool().unwrap();
            let name = t["name"].as_str().unwrap();
            assert_eq!(
                read_only,
                !matches!(name, "add_rule" | "remove_rule" | "set_setting"),
                "{name}"
            );
        }
        assert_eq!(
            list[5]["inputSchema"]["required"],
            json!(["route", "target"])
        );
        assert!(tools.call("cert", &json!({})).is_none());
    }
}
