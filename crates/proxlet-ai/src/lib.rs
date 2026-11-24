//! AI tool integrations.
//!
//! Detects installed AI coding tools and registers the proxlet MCP server with
//! them. Each tool lives in its own module behind the [`AiTool`] trait, so
//! adding a tool means adding one module and one line in [`tools`]; nothing
//! else in proxlet changes.

pub mod claude_code;
pub mod cursor;
mod json_file;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use proxlet_core::command::{CommandRunner, SystemRunner};

/// The name proxlet's server is registered under, in every tool.
pub const SERVER_NAME: &str = "proxlet";

/// Every supported tool's id, in the order [`tools`] lists them.
pub const TOOL_IDS: [&str; 2] = ["claude-code", "cursor"];

/// The MCP server to register: a command and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// Absolute path of the `proxlet` binary.
    pub command: PathBuf,
    /// Its arguments (`["mcp"]`).
    pub args: Vec<String>,
}

impl Server {
    /// `proxlet mcp`, run from `command`.
    #[must_use]
    pub fn proxlet(command: PathBuf) -> Self {
        Self {
            command,
            args: vec!["mcp".to_owned()],
        }
    }
}

/// Whether a tool is installed and has proxlet registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// The tool is not installed.
    NotInstalled,
    /// Installed, without a `proxlet` server.
    NotConnected,
    /// A `proxlet` server is registered, running `command`.
    Connected {
        /// The registered command.
        command: PathBuf,
    },
}

/// Why talking to a tool failed.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// Reading or writing a file failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A configuration file could not be understood; it was left alone.
    #[error("{} is not valid JSON ({message}); proxlet left it unchanged", path.display())]
    Parse {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
    /// A command failed.
    #[error("`{command}` failed{}", detail(.output))]
    Command {
        /// The command line.
        command: String,
        /// What it printed.
        output: String,
    },
}

fn detail(output: &str) -> String {
    let output = output.trim();
    if output.is_empty() {
        String::new()
    } else {
        format!(":\n{output}")
    }
}

/// One AI tool proxlet can register its MCP server with.
pub trait AiTool {
    /// Short id used on the command line, e.g. `cursor`.
    fn id(&self) -> &'static str;
    /// Display name, e.g. `Cursor`.
    fn name(&self) -> &'static str;
    /// Where the registration lives, for messages.
    fn location(&self) -> String;
    /// Whether the tool is installed and connected.
    fn status(&self) -> Result<Connection, AiError>;
    /// Registers (or re-registers) `server` at user scope.
    fn connect(&self, server: &Server) -> Result<(), AiError>;
    /// Removes the registration. `false` when there was none.
    fn disconnect(&self) -> Result<bool, AiError>;
}

/// The machine the tools are looked up on: the home directory, `PATH`, and how
/// commands are run.
pub struct Env {
    /// The user's home directory.
    pub home: PathBuf,
    /// The `PATH` to look programs up in.
    pub path: OsString,
    /// `CLAUDE_CONFIG_DIR`, if set.
    pub claude_config_dir: Option<PathBuf>,
    /// Runs external commands.
    pub runner: Box<dyn CommandRunner>,
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}

impl Env {
    /// This process's environment. `None` without a home directory.
    #[must_use]
    pub fn system() -> Option<Self> {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        Some(Self {
            home: PathBuf::from(home),
            path: std::env::var_os("PATH").unwrap_or_default(),
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
            runner: Box::new(SystemRunner),
        })
    }

    /// The first executable named `program` on `PATH`.
    #[must_use]
    pub fn find(&self, program: &str) -> Option<PathBuf> {
        std::env::split_paths(&self.path)
            .map(|dir| dir.join(program))
            .find(|p| is_executable(p))
    }

    /// `path` with the home directory written as `~`, for messages.
    #[must_use]
    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Every supported tool, in [`TOOL_IDS`] order.
#[must_use]
pub fn tools(env: &Env) -> Vec<Box<dyn AiTool + '_>> {
    vec![
        Box::new(claude_code::ClaudeCode::new(env)),
        Box::new(cursor::Cursor::new(env)),
    ]
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;
    use std::rc::Rc;

    use proxlet_core::command::{CommandOutput, CommandRunner};

    use super::*;

    /// Records every command and answers with `code` and `stderr`.
    #[derive(Clone, Default)]
    pub struct Recorder {
        pub calls: Rc<RefCell<Vec<Vec<String>>>>,
        pub code: Rc<RefCell<i32>>,
        pub stderr: Rc<RefCell<String>>,
    }

    impl CommandRunner for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().map(|a| (*a).to_owned()));
            self.calls.borrow_mut().push(call);
            Ok(CommandOutput {
                code: *self.code.borrow(),
                stdout: String::new(),
                stderr: self.stderr.borrow().clone(),
            })
        }
    }

    /// An environment in a temporary home, with an empty `bin/` on `PATH`.
    pub fn env(home: &Path, recorder: &Recorder) -> Env {
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        Env {
            home: home.to_path_buf(),
            path: bin.into_os_string(),
            claude_config_dir: None,
            runner: Box::new(recorder.clone()),
        }
    }

    /// Puts an executable `name` in the environment's `bin/`.
    pub fn install(home: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = home.join("bin").join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_listed_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let env = testing::env(dir.path(), &testing::Recorder::default());
        let ids: Vec<&str> = tools(&env).iter().map(|t| t.id()).collect();
        assert_eq!(ids, TOOL_IDS);
    }

    #[test]
    fn programs_are_found_on_path_only_when_executable() {
        let dir = tempfile::tempdir().unwrap();
        let env = testing::env(dir.path(), &testing::Recorder::default());
        assert_eq!(env.find("claude"), None);
        std::fs::write(dir.path().join("bin/claude"), "not executable").unwrap();
        assert_eq!(env.find("claude"), None);
        let path = testing::install(dir.path(), "claude");
        assert_eq!(env.find("claude"), Some(path));
        assert_eq!(
            env.display(&dir.path().join(".cursor/mcp.json")),
            "~/.cursor/mcp.json"
        );
    }
}
