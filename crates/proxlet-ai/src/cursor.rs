//! Cursor: the user-level `~/.cursor/mcp.json`, edited in place.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::{AiError, AiTool, Connection, Env, SERVER_NAME, Server, json_file};

/// Cursor's user-level MCP configuration.
#[derive(Debug)]
pub struct Cursor<'a> {
    env: &'a Env,
}

impl<'a> Cursor<'a> {
    /// Cursor on this machine.
    #[must_use]
    pub fn new(env: &'a Env) -> Self {
        Self { env }
    }

    fn dir(&self) -> PathBuf {
        self.env.home.join(".cursor")
    }

    fn file(&self) -> PathBuf {
        self.dir().join("mcp.json")
    }

    fn installed(&self) -> bool {
        self.dir().is_dir() || self.env.find("cursor").is_some()
    }
}

fn servers(root: &mut Map<String, Value>) -> Option<&mut Map<String, Value>> {
    root.get_mut("mcpServers").and_then(Value::as_object_mut)
}

impl AiTool for Cursor<'_> {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn location(&self) -> String {
        self.env.display(&self.file())
    }

    fn status(&self) -> Result<Connection, AiError> {
        if !self.installed() {
            return Ok(Connection::NotInstalled);
        }
        let mut doc = json_file::read(&self.file())?;
        let command = servers(&mut doc.root)
            .and_then(|s| s.get(SERVER_NAME))
            .and_then(|e| e.get("command"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        Ok(
            command.map_or(Connection::NotConnected, |command| Connection::Connected {
                command,
            }),
        )
    }

    fn connect(&self, server: &Server) -> Result<(), AiError> {
        let path = self.file();
        let mut doc = json_file::read(&path)?;
        let entry = json!({
            "command": server.command.to_string_lossy(),
            "args": server.args,
        });
        if servers(&mut doc.root).is_none() {
            doc.root
                .insert("mcpServers".into(), Value::Object(Map::new()));
        }
        let Some(list) = servers(&mut doc.root) else {
            return Err(AiError::Parse {
                path,
                message: "`mcpServers` is not an object".into(),
            });
        };
        list.insert(SERVER_NAME.into(), entry);
        json_file::write(&path, &doc)
    }

    fn disconnect(&self) -> Result<bool, AiError> {
        let path = self.file();
        if !path.exists() {
            return Ok(false);
        }
        let mut doc = json_file::read(&path)?;
        let removed = servers(&mut doc.root).and_then(|s| s.shift_remove(SERVER_NAME));
        if removed.is_none() {
            return Ok(false);
        }
        json_file::write(&path, &doc)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::testing;

    fn server() -> Server {
        Server::proxlet("/usr/local/bin/proxlet".into())
    }

    #[test]
    fn three_states() {
        let dir = tempfile::tempdir().unwrap();
        let env = testing::env(dir.path(), &testing::Recorder::default());
        let cursor = Cursor::new(&env);
        assert_eq!(cursor.status().unwrap(), Connection::NotInstalled);
        // Installed as a command on PATH, or by its directory.
        testing::install(dir.path(), "cursor");
        assert_eq!(cursor.status().unwrap(), Connection::NotConnected);
        std::fs::create_dir(dir.path().join(".cursor")).unwrap();
        assert_eq!(cursor.status().unwrap(), Connection::NotConnected);
        cursor.connect(&server()).unwrap();
        assert_eq!(
            cursor.status().unwrap(),
            Connection::Connected {
                command: "/usr/local/bin/proxlet".into()
            }
        );
        assert_eq!(cursor.location(), "~/.cursor/mcp.json");
    }

    #[test]
    fn file_created() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cursor")).unwrap();
        let env = testing::env(dir.path(), &testing::Recorder::default());
        Cursor::new(&env).connect(&server()).unwrap();
        let file = dir.path().join(".cursor/mcp.json");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\n  \"mcpServers\": {\n    \"proxlet\": {\n      \"command\": \"/usr/local/bin/proxlet\",\n      \"args\": [\n        \"mcp\"\n      ]\n    }\n  }\n}\n"
        );
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            !json_file::backup_path(&file).exists(),
            "nothing to back up"
        );
    }

    #[test]
    fn existing_servers_and_keys_are_kept_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cursor")).unwrap();
        let file = dir.path().join(".cursor/mcp.json");
        let before = r#"{"zeta": 1, "mcpServers": {"github": {"command": "gh-mcp", "args": ["--x"], "env": {"T": "1"}}}, "alpha": true}"#;
        std::fs::write(&file, before).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let env = testing::env(dir.path(), &testing::Recorder::default());
        let cursor = Cursor::new(&env);
        cursor.connect(&server()).unwrap();

        let after: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let keys: Vec<&String> = after.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["zeta", "mcpServers", "alpha"]);
        let names: Vec<&String> = after["mcpServers"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["github", "proxlet"]);
        assert_eq!(
            after["mcpServers"]["github"],
            json!({"command": "gh-mcp", "args": ["--x"], "env": {"T": "1"}})
        );
        assert_eq!(
            std::fs::read_to_string(json_file::backup_path(&file)).unwrap(),
            before
        );
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "mode kept");

        // Disconnecting keeps the rest.
        assert!(cursor.disconnect().unwrap());
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let names: Vec<&String> = after["mcpServers"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["github"]);
        assert_eq!(after["zeta"], 1);
        assert!(!cursor.disconnect().unwrap(), "nothing left to remove");
    }

    #[test]
    fn unreadable_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cursor")).unwrap();
        let file = dir.path().join(".cursor/mcp.json");
        std::fs::write(&file, "{ not json").unwrap();
        let env = testing::env(dir.path(), &testing::Recorder::default());
        let cursor = Cursor::new(&env);
        let err = cursor.connect(&server()).unwrap_err();
        assert!(matches!(err, AiError::Parse { .. }), "{err}");
        assert!(err.to_string().contains("mcp.json"), "{err}");
        assert!(err.to_string().contains("left it unchanged"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ not json");
        assert!(!json_file::backup_path(&file).exists());
        assert!(cursor.status().is_err());
        assert!(cursor.disconnect().is_err());
    }

    #[test]
    fn disconnect_without_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let env = testing::env(dir.path(), &testing::Recorder::default());
        assert!(!Cursor::new(&env).disconnect().unwrap());
    }
}
