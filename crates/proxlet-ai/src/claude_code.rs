//! Claude Code: registered with its own `claude mcp` commands at user scope.
//! Its configuration file is only read (for the status), never written:
//! Claude Code rewrites it often (design decision 4).

use std::path::PathBuf;

use serde_json::Value;

use crate::{AiError, AiTool, Connection, Env, SERVER_NAME, Server, json_file};

/// Claude Code's user-scope MCP servers.
#[derive(Debug)]
pub struct ClaudeCode<'a> {
    env: &'a Env,
}

impl<'a> ClaudeCode<'a> {
    /// Claude Code on this machine.
    #[must_use]
    pub fn new(env: &'a Env) -> Self {
        Self { env }
    }

    /// Where Claude Code keeps user-scope servers.
    fn file(&self) -> PathBuf {
        match &self.env.claude_config_dir {
            Some(dir) => dir.join(".claude.json"),
            None => self.env.home.join(".claude.json"),
        }
    }

    fn claude(&self) -> Option<String> {
        self.env
            .find("claude")
            .map(|p| p.to_string_lossy().into_owned())
    }

    fn registered(&self) -> Result<Option<PathBuf>, AiError> {
        let doc = json_file::read(&self.file())?;
        Ok(doc
            .root
            .get("mcpServers")
            .and_then(|s| s.get(SERVER_NAME))
            .and_then(|e| e.get("command"))
            .and_then(Value::as_str)
            .map(PathBuf::from))
    }

    fn run(&self, claude: &str, args: &[&str]) -> Result<(), AiError> {
        let line = format!("claude {}", args.join(" "));
        let out = self
            .env
            .runner
            .run(claude, args)
            .map_err(|e| AiError::Command {
                command: line.clone(),
                output: e.to_string(),
            })?;
        if out.code == 0 {
            Ok(())
        } else {
            Err(AiError::Command {
                command: line,
                output: format!("{}{}", out.stderr, out.stdout),
            })
        }
    }
}

impl AiTool for ClaudeCode<'_> {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn location(&self) -> String {
        format!("user scope, {}", self.env.display(&self.file()))
    }

    fn status(&self) -> Result<Connection, AiError> {
        if self.claude().is_none() {
            return Ok(Connection::NotInstalled);
        }
        Ok(match self.registered()? {
            Some(command) => Connection::Connected { command },
            None => Connection::NotConnected,
        })
    }

    fn connect(&self, server: &Server) -> Result<(), AiError> {
        let Some(claude) = self.claude() else {
            return Err(AiError::Command {
                command: "claude".into(),
                output: "Claude Code's `claude` command is not on PATH".into(),
            });
        };
        // `claude mcp add` refuses to replace an existing server.
        if self.registered()?.is_some() {
            self.run(&claude, &["mcp", "remove", "--scope", "user", SERVER_NAME])?;
        }
        let command = server.command.to_string_lossy();
        let mut args = vec!["mcp", "add", "--scope", "user", SERVER_NAME, "--", &command];
        args.extend(server.args.iter().map(String::as_str));
        self.run(&claude, &args)
    }

    fn disconnect(&self) -> Result<bool, AiError> {
        let Some(claude) = self.claude() else {
            return Ok(false);
        };
        if self.registered()?.is_none() {
            return Ok(false);
        }
        self.run(&claude, &["mcp", "remove", "--scope", "user", SERVER_NAME])?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn register(home: &std::path::Path, command: &str) {
        std::fs::write(
            home.join(".claude.json"),
            serde_json::json!({"numStartups": 3, "mcpServers": {"proxlet": {"type": "stdio", "command": command, "args": ["mcp"]}}}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn three_states_and_another_path() {
        let dir = tempfile::tempdir().unwrap();
        let rec = testing::Recorder::default();
        let env = testing::env(dir.path(), &rec);
        let cc = ClaudeCode::new(&env);
        assert_eq!(cc.status().unwrap(), Connection::NotInstalled);
        testing::install(dir.path(), "claude");
        assert_eq!(cc.status().unwrap(), Connection::NotConnected);
        register(dir.path(), "/old/proxlet");
        assert_eq!(
            cc.status().unwrap(),
            Connection::Connected {
                command: "/old/proxlet".into()
            }
        );
        assert!(rec.calls.borrow().is_empty(), "status runs nothing");
    }

    #[test]
    fn config_dir_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let rec = testing::Recorder::default();
        let mut env = testing::env(dir.path(), &rec);
        testing::install(dir.path(), "claude");
        let other = dir.path().join("claude-config");
        std::fs::create_dir(&other).unwrap();
        register(&other, "/opt/proxlet");
        env.claude_config_dir = Some(other);
        assert_eq!(
            ClaudeCode::new(&env).status().unwrap(),
            Connection::Connected {
                command: "/opt/proxlet".into()
            }
        );
    }

    #[test]
    fn connect_runs_claude_mcp_add() {
        let dir = tempfile::tempdir().unwrap();
        let rec = testing::Recorder::default();
        let env = testing::env(dir.path(), &rec);
        let claude = testing::install(dir.path(), "claude");
        let claude = claude.to_str().unwrap();
        let cc = ClaudeCode::new(&env);
        cc.connect(&Server::proxlet("/usr/local/bin/proxlet".into()))
            .unwrap();
        assert_eq!(
            *rec.calls.borrow(),
            [vec![
                claude,
                "mcp",
                "add",
                "--scope",
                "user",
                "proxlet",
                "--",
                "/usr/local/bin/proxlet",
                "mcp"
            ]]
        );

        // Registered elsewhere: removed first, then added.
        register(dir.path(), "/old/proxlet");
        rec.calls.borrow_mut().clear();
        cc.connect(&Server::proxlet("/usr/local/bin/proxlet".into()))
            .unwrap();
        let calls = rec.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0][1..],
            ["mcp", "remove", "--scope", "user", "proxlet"]
        );
        assert_eq!(calls[1][1..3], ["mcp", "add"]);
    }

    #[test]
    fn a_failing_claude_is_reported_with_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let rec = testing::Recorder::default();
        *rec.code.borrow_mut() = 1;
        *rec.stderr.borrow_mut() = "MCP server proxlet already exists".into();
        let env = testing::env(dir.path(), &rec);
        testing::install(dir.path(), "claude");
        let err = ClaudeCode::new(&env)
            .connect(&Server::proxlet("/p".into()))
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("`claude mcp add --scope user proxlet -- /p mcp` failed"),
            "{text}"
        );
        assert!(text.contains("already exists"), "{text}");
    }

    #[test]
    fn disconnect_only_when_registered() {
        let dir = tempfile::tempdir().unwrap();
        let rec = testing::Recorder::default();
        let env = testing::env(dir.path(), &rec);
        let cc = ClaudeCode::new(&env);
        assert!(!cc.disconnect().unwrap(), "not installed");
        testing::install(dir.path(), "claude");
        assert!(!cc.disconnect().unwrap(), "not registered");
        assert!(rec.calls.borrow().is_empty());
        register(dir.path(), "/p");
        assert!(cc.disconnect().unwrap());
        assert_eq!(
            rec.calls.borrow()[0][1..],
            ["mcp", "remove", "--scope", "user", "proxlet"]
        );
    }
}
