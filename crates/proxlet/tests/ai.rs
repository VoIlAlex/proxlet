//! `proxlet ai` and `proxlet ai rm`, with a fake `claude` and a temporary
//! home, so the developer's real AI tools are never touched.
#![allow(missing_docs)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::*;
use serde_json::{Value, json};

/// A fake `claude` that records its arguments and keeps `~/.claude.json` up
/// to date the way the real one does for user-scope servers.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
echo "$*" >> "$HOME/claude-calls.txt"
case "$1 $2" in
"mcp add") printf '{"numStartups":1,"mcpServers":{"proxlet":{"type":"stdio","command":"%s","args":["mcp"]}}}' "$7" > "$HOME/.claude.json" ;;
"mcp remove") printf '{"numStartups":1,"mcpServers":{}}' > "$HOME/.claude.json" ;;
esac
exit 0
"#;

struct Ai {
    home: Home,
}

impl Ai {
    /// A home with nothing installed.
    fn new() -> Self {
        let mut home = Home::new();
        let dir = home.path().to_path_buf();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        home.set_env("HOME", dir.to_str().unwrap());
        home.set_env(
            "PATH",
            &format!("{}:/usr/bin:/bin", dir.join("bin").display()),
        );
        Self { home }
    }

    fn with_claude(self) -> Self {
        let path = self.dir().join("bin/claude");
        std::fs::write(&path, FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        self
    }

    fn with_cursor(self) -> Self {
        std::fs::create_dir_all(self.dir().join(".cursor")).unwrap();
        self
    }

    fn dir(&self) -> &Path {
        self.home.path()
    }

    fn run(&self, args: &[&str]) -> Run {
        let out = self
            .home
            .command(args)
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn claude_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir().join("claude-calls.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn cursor_file(&self) -> PathBuf {
        self.dir().join(".cursor/mcp.json")
    }

    fn cursor_json(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.cursor_file()).unwrap()).unwrap()
    }
}

/// The binary the tests run, as `proxlet ai` registers it.
fn proxlet() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_proxlet"))
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn overview_then_connect() {
    let ai = Ai::new().with_claude().with_cursor();
    // Claude Code already connected to this binary.
    ai.run(&["ai", "claude-code", "--yes"]).ok();
    let out = ai.run(&["ai", "--yes"]);
    out.ok();
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert!(
        lines[0].contains("claude-code") && lines[0].contains("connected (user scope"),
        "{}",
        out.stdout
    );
    assert!(
        lines[1].contains("cursor") && lines[1].contains("not connected (~/.cursor/mcp.json)"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("Connected Cursor"), "{}", out.stdout);
    assert!(
        !out.stdout.contains("Connected Claude Code"),
        "{}",
        out.stdout
    );
    assert_eq!(
        ai.cursor_json(),
        json!({"mcpServers": {"proxlet": {"command": proxlet(), "args": ["mcp"]}}})
    );
}

#[test]
fn nothing_installed() {
    let ai = Ai::new();
    let out = ai.run(&["ai"]);
    out.ok();
    assert_eq!(
        out.stdout.matches("not installed").count(),
        2,
        "{}",
        out.stdout
    );
    assert!(out.stderr.contains("nothing to connect"), "{}", out.stderr);
}

#[test]
fn no_terminal_changes_nothing() {
    let ai = Ai::new().with_cursor();
    let out = ai.run(&["ai"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("proxlet ai --yes"), "{}", out.stderr);
    assert!(!ai.cursor_file().exists());
}

#[test]
fn one_tool() {
    let ai = Ai::new().with_claude().with_cursor();
    ai.run(&["ai", "cursor", "--yes"]).ok();
    assert!(ai.cursor_file().exists());
    assert!(ai.claude_calls().is_empty(), "{:?}", ai.claude_calls());
}

#[test]
fn already_connected() {
    let ai = Ai::new().with_claude().with_cursor();
    ai.run(&["ai", "--yes"]).ok();
    let calls = ai.claude_calls().len();
    // No --yes and no terminal: fine, because nothing needs asking.
    let out = ai.run(&["ai"]);
    out.ok();
    assert!(out.stderr.contains("Nothing to connect"), "{}", out.stderr);
    assert_eq!(ai.claude_calls().len(), calls);
}

#[test]
fn repairing_a_moved_binary() {
    let ai = Ai::new().with_claude().with_cursor();
    std::fs::write(
        ai.cursor_file(),
        r#"{"mcpServers":{"proxlet":{"command":"/gone/proxlet","args":["mcp"]}}}"#,
    )
    .unwrap();
    let out = ai.run(&["ai", "cursor"]);
    // Shown as connected elsewhere, and asked about.
    assert!(
        out.stdout.contains("connected to /gone/proxlet"),
        "{}",
        out.stdout
    );
    assert_eq!(out.code, 1);
    ai.run(&["ai", "cursor", "--yes"]).ok();
    assert_eq!(
        ai.cursor_json()["mcpServers"]["proxlet"]["command"],
        proxlet()
    );
}

#[test]
fn claude_code_is_registered_through_its_cli() {
    let ai = Ai::new().with_claude();
    ai.run(&["ai", "--yes"]).ok();
    assert_eq!(
        ai.claude_calls(),
        [format!("mcp add --scope user proxlet -- {} mcp", proxlet())]
    );
    // Moved binary: removed, then added again.
    std::fs::write(
        ai.dir().join(".claude.json"),
        r#"{"mcpServers":{"proxlet":{"command":"/gone/proxlet","args":["mcp"]}}}"#,
    )
    .unwrap();
    ai.run(&["ai", "--yes"]).ok();
    let calls = ai.claude_calls();
    assert_eq!(calls[1], "mcp remove --scope user proxlet");
    assert!(
        calls[2].starts_with("mcp add --scope user proxlet -- "),
        "{calls:?}"
    );
}

#[test]
fn a_failing_claude_is_reported() {
    let ai = Ai::new().with_claude();
    std::fs::write(
        ai.dir().join("bin/claude"),
        "#!/bin/sh\necho 'something went wrong' >&2\nexit 1\n",
    )
    .unwrap();
    let out = ai.run(&["ai", "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("claude mcp add --scope user proxlet"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("something went wrong"),
        "{}",
        out.stderr
    );
}

#[test]
fn existing_cursor_servers_are_kept_and_backed_up() {
    let ai = Ai::new().with_cursor();
    let before = r#"{"mcpServers":{"github":{"command":"gh-mcp"}},"other":1}"#;
    std::fs::write(ai.cursor_file(), before).unwrap();
    ai.run(&["ai", "--yes"]).ok();
    let after = ai.cursor_json();
    assert_eq!(after["mcpServers"]["github"], json!({"command": "gh-mcp"}));
    assert_eq!(after["other"], 1);
    assert_eq!(
        std::fs::read_to_string(ai.dir().join(".cursor/mcp.json.proxlet-backup")).unwrap(),
        before
    );
}

#[test]
fn an_unreadable_cursor_file_is_left_alone() {
    let ai = Ai::new().with_cursor();
    std::fs::write(ai.cursor_file(), "{ oops").unwrap();
    let out = ai.run(&["ai", "cursor", "--yes"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains(".cursor/mcp.json"), "{}", out.stderr);
    assert_eq!(std::fs::read_to_string(ai.cursor_file()).unwrap(), "{ oops");
}

#[test]
fn remove_everywhere() {
    let ai = Ai::new().with_claude().with_cursor();
    std::fs::write(
        ai.cursor_file(),
        r#"{"mcpServers":{"github":{"command":"gh-mcp"}}}"#,
    )
    .unwrap();
    ai.run(&["ai", "--yes"]).ok();
    let out = ai.run(&["ai", "rm"]);
    out.ok();
    assert!(
        out.stdout.contains("Disconnected Claude Code"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("Disconnected Cursor"), "{}", out.stdout);
    assert_eq!(
        ai.claude_calls().last().unwrap(),
        "mcp remove --scope user proxlet"
    );
    assert_eq!(
        ai.cursor_json(),
        json!({"mcpServers": {"github": {"command": "gh-mcp"}}})
    );
}

#[test]
fn removing_from_a_tool_that_is_not_connected() {
    let ai = Ai::new().with_cursor();
    let out = ai.run(&["ai", "rm", "cursor"]);
    out.ok();
    assert!(
        out.stderr.contains("Cursor was not connected"),
        "{}",
        out.stderr
    );
}

#[test]
fn tools_complete() {
    let ai = Ai::new();
    assert_eq!(
        ai.home.complete(&["ai", "rm", "c"]),
        ["claude-code", "cursor"]
    );
    assert_eq!(ai.home.complete(&["ai", "cu"]), ["cursor"]);
}
