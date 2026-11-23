//! Human-facing output: success lines, warnings, errors and hints.
//!
//! Colours are applied only when the stream is a terminal (and `NO_COLOR` is
//! unset); `anstream` strips them otherwise, so output stays grep-friendly.

use std::fmt::Display;
use std::io::IsTerminal;
use std::process::ExitCode;

use anstyle::{AnsiColor, Style};

/// Green, bold.
pub const OK: Style = AnsiColor::Green.on_default().bold();
/// Yellow, bold.
pub const WARN: Style = AnsiColor::Yellow.on_default().bold();
/// Red, bold.
pub const ERR: Style = AnsiColor::Red.on_default().bold();
/// Dimmed.
pub const DIM: Style = Style::new().dimmed();
/// Bold.
pub const BOLD: Style = Style::new().bold();

/// Prints `✓ message` to stdout.
pub fn success(message: impl Display) {
    anstream::println!("{OK}✓{OK:#} {message}");
}

/// Prints `! message` to stderr.
pub fn warn(message: impl Display) {
    anstream::eprintln!("{WARN}!{WARN:#} {message}");
}

/// Prints an indented, dimmed note to stderr.
pub fn note(message: impl Display) {
    anstream::eprintln!("  {DIM}{message}{DIM:#}");
}

/// Whether stdout is a terminal (tables and colours) or a pipe (plain lines).
#[must_use]
pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// A command failure: what went wrong, what to try next, and the exit code.
#[derive(Debug)]
pub struct Failure {
    /// One-line description.
    pub message: String,
    /// Suggestions, printed after the message.
    pub hints: Vec<String>,
    /// Process exit code.
    pub code: u8,
}

impl Failure {
    /// A failure with exit code 1.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            hints: Vec::new(),
            code: 1,
        }
    }

    /// A failure that has already been explained; only hints are printed.
    #[must_use]
    pub fn quiet() -> Self {
        Self::new(String::new())
    }

    /// Adds a hint.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }

    /// Sets the exit code.
    #[must_use]
    pub fn code(mut self, code: u8) -> Self {
        self.code = code;
        self
    }

    /// Prints the failure to stderr and returns its exit code.
    pub fn report(&self) -> ExitCode {
        if !self.message.is_empty() {
            anstream::eprintln!("{ERR}error:{ERR:#} {}", self.message);
        }
        for hint in &self.hints {
            anstream::eprintln!("  {DIM}{hint}{DIM:#}");
        }
        ExitCode::from(self.code)
    }
}

macro_rules! failure_from {
    ($($t:ty),* $(,)?) => {
        $(impl From<$t> for Failure {
            fn from(e: $t) -> Self {
                Self::new(e.to_string())
            }
        })*
    };
}

failure_from!(
    proxlet_core::RouteError,
    proxlet_core::TargetError,
    proxlet_core::SettingError,
    proxlet_core::ConfigError,
    proxlet_core::ChangeError,
    std::io::Error,
);

/// Result type for commands.
pub type CmdResult = Result<(), Failure>;
