//! System service manager integrations.
//!
//! `proxlet start` / `proxlet stop` go through the [`ServiceManager`] trait.
//! systemd is the first implementation; another manager (launchd, `OpenRC`,
//! ...) is a new module implementing the trait plus one line in
//! [`system_managers`], with no changes to the CLI.

pub mod systemd;

use std::path::PathBuf;
use std::process::Command;

/// The manager names proxlet supports, for error messages.
pub const SUPPORTED: &[&str] = &["systemd"];

/// Why a service operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// A management command exited unsuccessfully.
    #[error("`{command}` failed: {message}")]
    Command {
        /// The command line that failed.
        command: String,
        /// Its error output, or the exit status if it printed nothing.
        message: String,
    },
    /// Reading or writing a service file failed.
    #[error("{0}")]
    Io(String),
}

/// The user the service runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAs {
    /// User name.
    pub user: String,
    /// Primary group name.
    pub group: String,
}

/// Everything a manager needs to install the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Absolute path of the executable.
    pub exec: PathBuf,
    /// Arguments, e.g. `["run"]`.
    pub args: Vec<String>,
    /// Who to run as; `None` runs as the manager's default (root).
    pub run_as: Option<RunAs>,
    /// Environment for the process.
    pub env: Vec<(String, String)>,
    /// Whether the process must be able to bind ports below 1024.
    pub bind_privileged_ports: bool,
}

/// What the manager reports about the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is not installed.
    NotInstalled,
    /// Installed but not running.
    Stopped,
    /// Starting up.
    Starting,
    /// Exited with an error and waiting to be restarted.
    Restarting,
    /// Running.
    Running,
    /// Gave up after failing.
    Failed,
}

/// A system service manager.
pub trait ServiceManager {
    /// Short name, e.g. `systemd`.
    fn name(&self) -> &'static str;
    /// Whether this manager runs the host.
    fn is_present(&self) -> bool;
    /// Installs or updates the service definition. Returns whether anything changed.
    fn install(&self, spec: &ServiceSpec) -> Result<bool, ServiceError>;
    /// Enables the service at boot and starts it now.
    fn start(&self) -> Result<(), ServiceError>;
    /// Stops the service and disables it at boot.
    fn stop(&self) -> Result<(), ServiceError>;
    /// The service's current state.
    fn state(&self) -> Result<ServiceState, ServiceError>;
    /// The service's most recent log lines, newest last, for explaining failures.
    fn recent_logs(&self, lines: usize) -> Vec<String>;
}

/// Runs external commands. Injected so managers can be tested without a real host.
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

/// Every manager proxlet knows, in detection order.
#[must_use]
pub fn system_managers() -> Vec<Box<dyn ServiceManager>> {
    vec![Box::new(systemd::Systemd::new(SystemRunner))]
}

/// The first manager in `candidates` that reports itself present.
#[must_use]
pub fn detect(candidates: Vec<Box<dyn ServiceManager>>) -> Option<Box<dyn ServiceManager>> {
    candidates.into_iter().find(|m| m.is_present())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(&'static str, bool);

    impl ServiceManager for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        fn is_present(&self) -> bool {
            self.1
        }
        fn install(&self, _: &ServiceSpec) -> Result<bool, ServiceError> {
            Ok(false)
        }
        fn start(&self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn stop(&self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn state(&self) -> Result<ServiceState, ServiceError> {
            Ok(ServiceState::NotInstalled)
        }
        fn recent_logs(&self, _: usize) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn detect_picks_first_present_manager() {
        let found = detect(vec![
            Box::new(Fake("absent", false)),
            Box::new(Fake("first", true)),
            Box::new(Fake("second", true)),
        ]);
        assert_eq!(found.map(|m| m.name()), Some("first"));
    }

    #[test]
    fn detect_none_found() {
        assert!(detect(vec![Box::new(Fake("absent", false))]).is_none());
        assert!(detect(Vec::new()).is_none());
    }
}
