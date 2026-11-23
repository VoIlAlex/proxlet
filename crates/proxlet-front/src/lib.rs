//! Front-server integrations for attach mode.
//!
//! Each supported front server (nginx first) lives in its own module behind the
//! [`FrontServer`] trait, so adding another server such as Apache never touches
//! the engine, the CLI or the service managers. Applying and undoing changes to
//! a front server's config is generic: see [`apply`].

pub mod apply;
pub mod config;
pub mod nginx;

use std::net::SocketAddr;
use std::path::PathBuf;

use proxlet_core::port;

pub use apply::{Ledger, Step};
pub use config::{FrontConfig, Listen, Location, ServerBlock};
pub use nginx::Nginx;

/// Why a front-server operation failed.
#[derive(Debug, thiserror::Error)]
pub enum FrontError {
    /// Reading or writing a file failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The front server's config could not be read.
    #[error("{at}: {message}")]
    Parse {
        /// Where.
        at: Location,
        /// What is wrong.
        message: String,
    },
    /// A command could not be run or failed.
    #[error("`{command}` failed{}", detail(.output))]
    Command {
        /// The command line.
        command: String,
        /// What it printed.
        output: String,
    },
    /// The config layout has no place proxlet can use.
    #[error("{0}")]
    Layout(String),
}

fn detail(output: &str) -> String {
    let output = output.trim();
    if output.is_empty() {
        String::new()
    } else {
        format!(":\n{output}")
    }
}

/// A detected front-server installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    /// The executable.
    pub binary: PathBuf,
    /// Version string, e.g. `1.24.0`.
    pub version: String,
    /// The main config file.
    pub conf_path: PathBuf,
    /// The installation prefix, which relative paths in the config resolve against.
    pub prefix: PathBuf,
    /// The error log compiled in as the default.
    pub default_error_log: Option<PathBuf>,
}

/// What proxlet should change in a front server's config to attach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The steps, applied in order and undone in reverse.
    pub steps: Vec<Step>,
}

impl Plan {
    /// Whether the plan changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// One front server proxlet can attach behind.
pub trait FrontServer: Send + Sync {
    /// Short name shown to users, e.g. `nginx`.
    fn name(&self) -> &'static str;
    /// Finds the installation, if the front server is installed.
    fn detect(&self) -> Option<Installation>;
    /// Whether the front server's main process is running.
    fn is_running(&self) -> bool;
    /// The service unit that runs it, for hooks, e.g. `nginx.service`.
    fn service_unit(&self) -> Option<&'static str>;
    /// Reads the config the installation uses.
    ///
    /// # Errors
    ///
    /// When a config file cannot be read or parsed.
    fn read(&self, inst: &Installation) -> Result<FrontConfig, FrontError>;
    /// What to write so that every request the front server does not claim on
    /// `port` goes to `internal`. With `take_default`, also moves an existing
    /// default-server flag to proxlet's block; without it and with such a flag
    /// present, the plan is empty.
    ///
    /// # Errors
    ///
    /// When the config has no included directory proxlet can write into.
    fn plan_attach(
        &self,
        cfg: &FrontConfig,
        port: u16,
        internal: SocketAddr,
        take_default: bool,
    ) -> Result<Plan, FrontError>;
    /// Checks the config, e.g. `nginx -t`.
    ///
    /// # Errors
    ///
    /// With the front server's own message when the config is invalid.
    fn check(&self, inst: &Installation) -> Result<(), FrontError>;
    /// Makes a running front server load its config again.
    ///
    /// # Errors
    ///
    /// With the front server's own message when the reload fails.
    fn reload(&self, inst: &Installation) -> Result<(), FrontError>;
    /// The error log the running config writes to.
    fn error_log(&self, inst: &Installation, cfg: &FrontConfig) -> Option<PathBuf>;
    /// Whether a log line says the front server failed to bind `port` because it is taken.
    fn is_bind_failure(&self, line: &str, port: u16) -> bool;
    /// Whether the process holding `port` is this front server.
    ///
    /// `holder` is the identified holder, if any. When it is unknown (a
    /// non-root user cannot see root's sockets), a running front server whose
    /// config listens on `port` counts.
    fn holds(&self, holder: Option<&port::Holder>, port: u16, cfg: Option<&FrontConfig>) -> bool;
}
