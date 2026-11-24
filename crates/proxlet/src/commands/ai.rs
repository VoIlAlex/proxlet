//! `proxlet ai [<tool>…] [--yes]` and `proxlet ai rm [<tool>…]` (design
//! decision 5).

use proxlet_ai::{AiError, AiTool, Connection, Env, Server, TOOL_IDS};

use crate::output::{self, BOLD, CmdResult, DIM, Failure, OK, WARN};

/// `proxlet ai [<tool>…] [--yes]`.
pub fn connect(names: &[String], yes: bool) -> CmdResult {
    let env = system_env()?;
    let server = this_server()?;
    connect_with(&env, &server, names, yes, &output::confirm)
}

/// `proxlet ai rm [<tool>…]`.
pub fn rm(names: &[String]) -> CmdResult {
    let env = system_env()?;
    rm_with(&env, names)
}

fn system_env() -> Result<Env, Failure> {
    Env::system()
        .ok_or_else(|| Failure::new("HOME is not set, so proxlet cannot find your AI tools"))
}

/// `proxlet mcp`, run from this binary.
fn this_server() -> Result<Server, Failure> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| Failure::new(format!("cannot tell where this proxlet binary is: {e}")))?;
    Ok(Server::proxlet(exe))
}

/// The tools named on the command line, or all of them.
fn selected<'a>(env: &'a Env, names: &[String]) -> Result<Vec<Box<dyn AiTool + 'a>>, Failure> {
    if let Some(unknown) = names.iter().find(|n| !TOOL_IDS.contains(&n.as_str())) {
        return Err(Failure::new(format!("unknown AI tool `{unknown}`"))
            .hint(format!("Supported tools: {}", TOOL_IDS.join(", "))));
    }
    Ok(proxlet_ai::tools(env)
        .into_iter()
        .filter(|t| names.is_empty() || names.iter().any(|n| n == t.id()))
        .collect())
}

/// One overview line per tool, such as `✓ claude-code  connected`.
fn describe(env: &Env, tool: &dyn AiTool, status: &Result<Connection, AiError>, server: &Server) {
    let id = tool.id();
    let width = TOOL_IDS.iter().map(|i| i.len()).max().unwrap_or(0);
    match status {
        Ok(Connection::NotInstalled) => {
            anstream::println!("{DIM}· {id:<width$}  not installed{DIM:#}");
        }
        Ok(Connection::NotConnected) => {
            anstream::println!(
                "○ {BOLD}{id:<width$}{BOLD:#}  not connected {DIM}({}){DIM:#}",
                tool.location()
            );
        }
        Ok(Connection::Connected { command }) if *command == server.command => {
            anstream::println!(
                "{OK}✓{OK:#} {BOLD}{id:<width$}{BOLD:#}  connected {DIM}({}){DIM:#}",
                tool.location()
            );
        }
        Ok(Connection::Connected { command }) => {
            anstream::println!(
                "{WARN}!{WARN:#} {BOLD}{id:<width$}{BOLD:#}  connected to {}",
                env.display(command)
            );
        }
        Err(e) => anstream::println!("{WARN}!{WARN:#} {BOLD}{id:<width$}{BOLD:#}  {e}"),
    }
}

/// Whether `status` means the tool should be (re)connected.
fn needs_connecting(status: &Result<Connection, AiError>, server: &Server) -> bool {
    match status {
        Ok(Connection::NotConnected) | Err(_) => true,
        Ok(Connection::Connected { command }) => *command != server.command,
        Ok(Connection::NotInstalled) => false,
    }
}

pub(crate) fn connect_with(
    env: &Env,
    server: &Server,
    names: &[String],
    yes: bool,
    confirm: &dyn Fn(&str) -> Option<bool>,
) -> CmdResult {
    let tools = selected(env, names)?;
    let statuses: Vec<_> = tools.iter().map(|t| t.status()).collect();
    for (tool, status) in tools.iter().zip(&statuses) {
        describe(env, tool.as_ref(), status, server);
    }
    let targets: Vec<&dyn AiTool> = tools
        .iter()
        .zip(&statuses)
        .filter(|(_, s)| needs_connecting(s, server))
        .map(|(t, _)| t.as_ref())
        .collect();
    if targets.is_empty() {
        if statuses
            .iter()
            .all(|s| matches!(s, Ok(Connection::NotInstalled)))
        {
            output::note("No supported AI tool is installed, so there is nothing to connect.");
        } else {
            output::note("Nothing to connect.");
        }
        return Ok(());
    }
    let list = targets
        .iter()
        .map(|t| t.name())
        .collect::<Vec<_>>()
        .join(" and ");
    if !yes {
        match confirm(&format!("Connect proxlet to {list}?")) {
            Some(true) => {}
            Some(false) => {
                output::note("Nothing was changed.");
                return Ok(());
            }
            None => {
                let mut again = String::from("proxlet ai");
                for n in names {
                    again.push(' ');
                    again.push_str(n);
                }
                return Err(Failure::new("not connecting AI tools without confirmation")
                    .hint(format!("To connect them: {again} --yes")));
            }
        }
    }
    let mut failed = 0;
    for tool in targets {
        match tool.connect(server) {
            Ok(()) => output::success(format!("Connected {} ({})", tool.name(), tool.location())),
            Err(e) => {
                failed += 1;
                anstream::eprintln!("{WARN}!{WARN:#} {}: {e}", tool.name());
            }
        }
    }
    if failed > 0 {
        return Err(Failure::quiet());
    }
    output::note("Restart the AI tool (or reload its MCP servers) to use proxlet's tools.");
    Ok(())
}

pub(crate) fn rm_with(env: &Env, names: &[String]) -> CmdResult {
    let tools = selected(env, names)?;
    let mut failed = 0;
    let mut removed = 0;
    for tool in &tools {
        match tool.disconnect() {
            Ok(true) => {
                removed += 1;
                output::success(format!(
                    "Disconnected {} ({})",
                    tool.name(),
                    tool.location()
                ));
            }
            Ok(false) => {
                if !names.is_empty() {
                    output::note(format!("{} was not connected", tool.name()));
                }
            }
            Err(e) => {
                failed += 1;
                anstream::eprintln!("{WARN}!{WARN:#} {}: {e}", tool.name());
            }
        }
    }
    if failed > 0 {
        return Err(Failure::quiet());
    }
    if removed == 0 && names.is_empty() {
        output::note("proxlet was not connected to any AI tool.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use proxlet_core::command::{CommandOutput, CommandRunner};

    use super::*;

    struct NoCommands;

    impl CommandRunner for NoCommands {
        fn run(&self, program: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
            panic!("ran {program}");
        }
    }

    /// A home where only Cursor is installed (by its directory).
    fn cursor_home() -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cursor")).unwrap();
        let env = Env {
            home: dir.path().to_path_buf(),
            path: dir.path().join("no-bin").into_os_string(),
            claude_config_dir: None,
            runner: Box::new(NoCommands),
        };
        (dir, env)
    }

    fn server() -> Server {
        Server::proxlet("/usr/local/bin/proxlet".into())
    }

    fn registered(env: &Env, id: &str) -> Option<PathBuf> {
        proxlet_ai::tools(env)
            .into_iter()
            .find(|t| t.id() == id)
            .and_then(|t| match t.status() {
                Ok(Connection::Connected { command }) => Some(command),
                _ => None,
            })
    }

    #[test]
    fn confirmed() {
        let (_dir, env) = cursor_home();
        connect_with(&env, &server(), &[], false, &|q| {
            assert_eq!(q, "Connect proxlet to Cursor?");
            Some(true)
        })
        .unwrap();
        assert_eq!(
            registered(&env, "cursor"),
            Some("/usr/local/bin/proxlet".into())
        );
    }

    #[test]
    fn declined_changes_nothing() {
        let (dir, env) = cursor_home();
        connect_with(&env, &server(), &[], false, &|_| Some(false)).unwrap();
        assert!(!dir.path().join(".cursor/mcp.json").exists());
    }

    #[test]
    fn no_terminal_changes_nothing() {
        let (dir, env) = cursor_home();
        let err = connect_with(&env, &server(), &["cursor".into()], false, &|_| None).unwrap_err();
        assert_eq!(err.hints, ["To connect them: proxlet ai cursor --yes"]);
        assert!(!dir.path().join(".cursor/mcp.json").exists());
    }

    #[test]
    fn already_connected_asks_nothing() {
        let (_dir, env) = cursor_home();
        connect_with(&env, &server(), &[], true, &|_| None).unwrap();
        connect_with(&env, &server(), &[], false, &|_| panic!("asked")).unwrap();
        // Another binary: connected again.
        let moved = Server::proxlet("/new/proxlet".into());
        connect_with(&env, &moved, &[], true, &|_| panic!("asked despite --yes")).unwrap();
        assert_eq!(registered(&env, "cursor"), Some("/new/proxlet".into()));
    }

    #[test]
    fn unknown_tools_are_rejected() {
        let (_dir, env) = cursor_home();
        let err = connect_with(&env, &server(), &["vim".into()], true, &|_| None).unwrap_err();
        assert_eq!(err.message, "unknown AI tool `vim`");
        assert!(rm_with(&env, &["vim".into()]).is_err());
    }

    #[test]
    fn rm_removes_only_proxlet() {
        let (_dir, env) = cursor_home();
        connect_with(&env, &server(), &[], true, &|_| None).unwrap();
        rm_with(&env, &[]).unwrap();
        assert_eq!(registered(&env, "cursor"), None);
        rm_with(&env, &["cursor".into()]).unwrap();
    }
}
