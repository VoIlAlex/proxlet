//! `proxlet mcp`, driven over its stdio like an AI tool would.
#![allow(missing_docs)]
// The helpers take `json!` values by value to keep call sites readable.
#![allow(clippy::needless_pass_by_value)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(home: &Home) -> Self {
        let mut child = home
            .command(&["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next: 1,
        };
        let init = mcp.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        mcp.send_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        mcp
    }

    fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "server closed stdout"
        );
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send_line(&msg.to_string());
        let reply = self.read();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// Calls a tool and returns whether it failed, and the parsed result or the
    /// error text.
    fn call(&mut self, tool: &str, args: Value) -> (bool, Value) {
        let reply = self.request("tools/call", json!({"name": tool, "arguments": args}));
        let result = &reply["result"];
        let is_error = result["isError"].as_bool().expect("isError");
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        if is_error {
            (true, Value::String(text))
        } else {
            (false, serde_json::from_str(&text).unwrap())
        }
    }

    fn ok(&mut self, tool: &str, args: Value) -> Value {
        let (err, v) = self.call(tool, args);
        assert!(!err, "{tool} failed: {v}");
        v
    }

    fn err(&mut self, tool: &str, args: Value) -> String {
        let (err, v) = self.call(tool, args);
        assert!(err, "{tool} should fail: {v}");
        v.as_str().unwrap().to_owned()
    }

    /// Closes stdin and returns the exit code.
    fn finish(mut self) -> i32 {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            assert!(Instant::now() < deadline, "mcp did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn handshake_errors_and_exit() {
    let home = Home::new();
    let mut mcp = Mcp::start(&home);
    let r = mcp.request("initialize", json!({"protocolVersion": "1999-01-01"}));
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(r["result"]["serverInfo"]["name"], "proxlet");
    assert!(r["result"]["capabilities"]["tools"].is_object());

    let r = mcp.request("resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    mcp.send_line("this is not json");
    let r = mcp.read();
    assert_eq!(r["error"]["code"], -32700);
    assert_eq!(r["id"], Value::Null);
    // Still serving.
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));
    assert_eq!(mcp.finish(), 0);
}

#[test]
fn only_protocol_messages_on_stdout() {
    let home = Home::new();
    let mut child = home
        .command(&["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"get_stats"}}}}"#
        )
        .unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    for line in lines {
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
    }
}

#[test]
fn listing_tools() {
    let home = Home::new();
    let mut mcp = Mcp::start(&home);
    let r = mcp.request("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
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
    for t in tools {
        let name = t["name"].as_str().unwrap();
        let writes = matches!(name, "add_rule" | "remove_rule" | "set_setting");
        assert_eq!(t["annotations"]["readOnlyHint"], !writes, "{name}");
    }
}

#[test]
fn reading_without_a_server() {
    let home = Home::new();
    let site = tempfile::tempdir().unwrap();
    let site_path = site.path().canonicalize().unwrap();
    home.run(&["add", "/api", "8000"]).ok();
    home.run(&["add", "/docs", site_path.to_str().unwrap()])
        .ok();
    let mut mcp = Mcp::start(&home);

    assert_eq!(mcp.ok("status", json!({}))["running"], false);
    let rules = mcp.ok("list_rules", json!({}));
    assert_eq!(
        rules,
        json!([
            {"route": "/api", "kind": "proxy", "target": "http://127.0.0.1:8000"},
            {"route": "/docs", "kind": "files", "target": site_path.to_str().unwrap()}
        ])
    );
    let settings = mcp.ok("get_settings", json!({}));
    let listing = settings
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "static.listing")
        .unwrap()
        .clone();
    assert_eq!(listing["value"], listing["default"]);
    assert!(listing["help"].as_str().is_some_and(|h| !h.is_empty()));
    assert_eq!(mcp.ok("read_logs", json!({})), json!([]));

    let e = mcp.err("get_stats", json!({}));
    assert!(
        e.contains("proxlet is not running") && e.contains("proxlet start"),
        "{e}"
    );
}

#[test]
fn logs_for_a_route_and_bad_selectors() {
    let home = Home::new();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    home.run(&["add", "/docs", &free_port().to_string()]).ok();
    let daemon = home.start();
    for _ in 0..4 {
        home.get("x", "/api/users");
        home.get("x", "/docs/");
    }
    daemon.stop();
    let mut mcp = Mcp::start(&home);
    let records = mcp.ok("read_logs", json!({"selector": "/api", "limit": 3}));
    let records = records.as_array().unwrap();
    assert_eq!(records.len(), 3, "{records:?}");
    for r in records {
        assert_eq!(r["rule"], "/api", "{r}");
        assert_eq!(r["kind"], "request");
    }
    // Without a selector, events are included.
    let all = mcp.ok("read_logs", json!({"limit": 500}));
    assert!(all.as_array().unwrap().iter().any(|r| r["kind"] == "event"));

    let cli = home.run(&["logs", "a b c", "--all"]);
    let e = mcp.err("read_logs", json!({"selector": "a b c"}));
    assert!(
        cli.stderr.contains(e.lines().next().unwrap()),
        "{e} vs {}",
        cli.stderr
    );
    let e = mcp.err("read_logs", json!({"limit": 0}));
    assert!(e.contains("from 1 to 500"), "{e}");
}

#[test]
fn status_and_stats_with_a_server() {
    let home = Home::new();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    let _daemon = home.start();
    home.get("x", "/api");
    let mut mcp = Mcp::start(&home);
    let status = mcp.ok("status", json!({}));
    assert_eq!(status["running"], true);
    assert_eq!(status["rules"], 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = mcp.ok("get_stats", json!({}));
        let api = snapshot["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["rule"] == "/api")
            .unwrap()
            .clone();
        if api["total"] == 1 {
            assert_eq!(api["upstream_errors"], 1);
            break;
        }
        assert!(Instant::now() < deadline, "{snapshot}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn changes_apply_live() {
    let home = Home::new();
    let site = tempfile::tempdir().unwrap();
    std::fs::write(site.path().join("index.html"), "hello from mcp").unwrap();
    let site_path = site.path().canonicalize().unwrap();
    let _daemon = home.start();
    let mut mcp = Mcp::start(&home);

    let added = mcp.ok(
        "add_rule",
        json!({"route": "/site", "target": site_path.to_str().unwrap()}),
    );
    assert_eq!(added["outcome"], "added");
    assert_eq!(added["live"], true);
    assert_eq!(home.get("x", "/site/"), (200, "hello from mcp".to_owned()));

    let updated = mcp.ok("add_rule", json!({"route": "/site", "target": "8000"}));
    assert_eq!(updated["outcome"], "updated");
    assert_eq!(updated["previous"], site_path.to_str().unwrap());

    let set = mcp.ok(
        "set_setting",
        json!({"key": "static.listing", "value": "off"}),
    );
    assert_eq!(set["outcome"], "setting_changed");
    assert_eq!(set["live"], true);
    assert_eq!(
        home.run(&["config", "static.listing"]).ok().stdout.trim(),
        "off"
    );

    let removed = mcp.ok("remove_rule", json!({"route": "/site"}));
    assert_eq!(removed["outcome"], "removed");
    assert!(home.run(&["ls"]).stderr.contains("No rules yet"));
}

#[test]
fn invalid_changes_write_nothing() {
    let home = Home::new();
    let before = std::fs::read_to_string(home.config_file()).unwrap_or_default();
    let mut mcp = Mcp::start(&home);

    let cli = home.run(&["config", "log.max_size", "1MB"]);
    let e = mcp.err(
        "set_setting",
        json!({"key": "log.max_size", "value": "1MB"}),
    );
    assert!(
        cli.stderr.contains(e.lines().next().unwrap()),
        "{e} vs {}",
        cli.stderr
    );

    let e = mcp.err("remove_rule", json!({"route": "/nope"}));
    assert!(e.contains("there is no rule for `/nope`"), "{e}");

    let e = mcp.err("add_rule", json!({"route": "/docs", "target": "./site"}));
    assert!(e.contains("absolute path"), "{e}");
    let e = mcp.err("add_rule", json!({"route": "/docs"}));
    assert!(e.contains("needs `target`"), "{e}");

    assert_eq!(
        std::fs::read_to_string(home.config_file()).unwrap_or_default(),
        before
    );
}

#[test]
fn changes_while_stopped_apply_at_start() {
    let home = Home::new();
    let mut mcp = Mcp::start(&home);
    let added = mcp.ok("add_rule", json!({"route": "/api", "target": "8000"}));
    assert_eq!(added["live"], false);
    assert!(
        added["note"]
            .as_str()
            .unwrap()
            .contains("applies when it starts")
    );
    assert!(home.run(&["ls"]).ok().stdout.starts_with("/api\t"));
}

#[test]
fn hosts_nginx_serves_need_allowing() {
    let home = Home::with_nginx(false);
    let mut mcp = Mcp::start(&home);
    let e = mcp.err(
        "add_rule",
        json!({"route": "site.example", "target": "3000"}),
    );
    assert!(e.starts_with("nginx serves site.example"), "{e}");
    assert!(e.contains("allow_front_conflict"), "{e}");
    assert!(home.run(&["ls"]).stderr.contains("No rules yet"));

    let added = mcp.ok(
        "add_rule",
        json!({"route": "site.example", "target": "3000", "allow_front_conflict": true}),
    );
    assert_eq!(added["outcome"], "added");
    // Hosts nginx does not serve need nothing.
    mcp.ok(
        "add_rule",
        json!({"route": "other.example", "target": "3001"}),
    );
}
