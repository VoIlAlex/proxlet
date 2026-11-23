//! Running external commands, behind a trait so callers can be tested
//! without the real tools.

use std::process::Command;

/// Runs external commands. Injected so integrations can be tested without a real host.
pub trait CommandRunner {
    /// Runs `program args...` and returns its exit code and output.
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput>;
}

/// The result of running a command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code (`-1` if killed by a signal).
    pub code: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// Runs commands on the real system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        let out = Command::new(program).args(args).output()?;
        Ok(CommandOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}
