//! `proxlet mcp`: a Model Context Protocol server over stdio (design
//! decision 1).
//!
//! One JSON-RPC 2.0 message per line. Requests get exactly one reply,
//! notifications none. Only protocol messages are written to the output;
//! diagnostics go to stderr.

pub mod tools;

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

use crate::output::Failure;

/// Protocol versions proxlet speaks, oldest first.
pub const VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// What the server exposes as tools.
pub trait Tools {
    /// The `tools/list` entries.
    fn list(&self) -> Vec<Value>;
    /// Runs a tool; `None` when no tool has that name.
    fn call(&self, name: &str, args: &Value) -> Option<Result<Value, Failure>>;
}

/// Serves requests from `input` until it ends.
pub fn serve(input: impl BufRead, mut output: impl Write, tools: &dyn Tools) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(&line, tools) {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// The reply to one line, if it needs one.
fn handle(line: &str, tools: &dyn Tools) -> Option<Value> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(error(&Value::Null, PARSE_ERROR, "parse error"));
    };
    let Some(obj) = msg.as_object() else {
        return Some(error(&Value::Null, INVALID_REQUEST, "invalid request"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response to something we never sent, or garbage without an id.
        return id.map(|id| error(&id, INVALID_REQUEST, "invalid request"));
    };
    // Notifications (no id) never get a reply.
    let id = id?;
    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
    Some(match dispatch(method, &params, tools) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => error(&id, code, &message),
    })
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn dispatch(method: &str, params: &Value, tools: &dyn Tools) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools.list()})),
        "tools/call" => call(params, tools),
        other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| VERSIONS.contains(v))
        .unwrap_or(VERSIONS[VERSIONS.len() - 1]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "proxlet", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "proxlet is a local reverse proxy and file server. Rules map a route \
            (`/api`, `example.com/api`) to a target (a port, host:port, URL or absolute \
            directory). Read with status, list_rules, get_settings, read_logs and get_stats; \
            change with add_rule, remove_rule and set_setting."
    })
}

fn call(params: &Value, tools: &dyn Tools) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((INVALID_PARAMS, "tools/call needs a tool name".into()));
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err((INVALID_PARAMS, "arguments must be an object".into()));
    }
    match tools.call(name, &args) {
        None => Err((INVALID_PARAMS, format!("unknown tool: {name}"))),
        Some(Ok(value)) => Ok(json!({
            "content": [{"type": "text", "text": pretty(&value)}],
            "structuredContent": structured(value),
            "isError": false
        })),
        Some(Err(failure)) => Ok(json!({
            "content": [{"type": "text", "text": failure_text(&failure)}],
            "isError": true
        })),
    }
}

/// `structuredContent` must be an object; arrays are wrapped.
fn structured(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({"result": value})
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The CLI's message and hints, one per line.
#[must_use]
pub fn failure_text(failure: &Failure) -> String {
    let mut text = failure.message.clone();
    for hint in &failure.hints {
        text.push('\n');
        text.push_str(hint);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Tools for Echo {
        fn list(&self) -> Vec<Value> {
            vec![json!({"name": "echo"})]
        }
        fn call(&self, name: &str, args: &Value) -> Option<Result<Value, Failure>> {
            match name {
                "echo" => Some(Ok(args.clone())),
                "list" => Some(Ok(json!([1, 2]))),
                "fail" => Some(Err(Failure::new("it broke").hint("try again"))),
                _ => None,
            }
        }
    }

    fn run(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, &Echo).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn one(input: &str) -> Value {
        let mut replies = run(input);
        assert_eq!(replies.len(), 1, "{replies:?}");
        replies.remove(0)
    }

    #[test]
    fn initialize_negotiates_the_version() {
        let r = one(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        );
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "proxlet");
        assert!(r["result"]["capabilities"]["tools"].is_object());

        let r = one(
            r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(r["id"], "a");
        assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn notifications_get_no_reply_and_ping_answers() {
        let replies = run(concat!(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n\n",
            r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#,
            "\n"
        ));
        assert_eq!(replies, [json!({"jsonrpc":"2.0","id":7,"result":{}})]);
    }

    #[test]
    fn errors_keep_the_server_running() {
        let replies = run(concat!(
            "not json\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#,
            "\n",
            "[1,2]\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#,
            "\n"
        ));
        let codes: Vec<_> = replies.iter().map(|r| r["error"]["code"].clone()).collect();
        assert_eq!(
            codes,
            [
                json!(-32700),
                json!(-32601),
                json!(-32600),
                json!(-32602),
                json!(-32602),
                Value::Null
            ]
        );
        assert_eq!(replies[0]["id"], Value::Null);
        assert_eq!(replies[5]["result"], json!({}));
    }

    #[test]
    fn tool_results_and_failures() {
        let r = one(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"a":1}}}"#,
        );
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["structuredContent"], json!({"a": 1}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({"a": 1})
        );

        let r = one(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list"}}"#);
        assert_eq!(r["result"]["structuredContent"], json!({"result": [1, 2]}));

        let r = one(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fail"}}"#);
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["result"]["content"][0]["text"], "it broke\ntry again");

        let r = one(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        assert_eq!(r["result"]["tools"][0]["name"], "echo");
    }
}
