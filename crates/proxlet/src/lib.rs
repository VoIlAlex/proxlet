//! proxlet — a tiny, fast reverse proxy and HTTP server.
//!
//! The binary is both the CLI and the daemon. This library holds everything
//! behind `main` so integration tests can drive it.

pub mod cli;
pub mod commands;
pub mod complete;
pub mod control;
pub mod daemon;
pub mod output;
pub mod pipeline;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use proxlet_core::Locations;

use crate::cli::{Cli, Command};
use crate::output::{CmdResult, Failure};

/// Entry point for the `proxlet` binary.
pub fn main() -> ExitCode {
    // Serves shell completions when invoked by the shell (`COMPLETE=zsh proxlet`).
    CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();
    let locations = Locations::resolve();
    match dispatch(&cli.command, &locations) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => failure.report(),
    }
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("PROXLET_LOG")
        .unwrap_or_else(|_| EnvFilter::new("proxlet=info,proxlet_server=info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr);
    // The journal timestamps lines itself.
    if std::env::var_os(daemon::SERVICE_ENV).is_some() {
        builder.without_time().init();
    } else {
        builder.init();
    }
}

fn dispatch(command: &Command, locations: &Locations) -> CmdResult {
    match command {
        Command::Run => {
            init_logging();
            daemon::run(locations)
        }
        Command::Start => commands::service::start(locations),
        Command::Stop => commands::service::stop(locations),
        Command::Status => commands::status::run(locations),
        Command::Add { route, target } => commands::rules::add(locations, route, target),
        Command::Rm { route } => commands::rules::rm(locations, route),
        Command::Ls => commands::rules::ls(locations),
        Command::Config { key, value } => {
            commands::config::run(locations, key.as_deref(), value.as_deref())
        }
        Command::Logs { .. } => not_yet("logs", "structured logs"),
        Command::Cert { .. } => not_yet("cert", "certificates"),
        Command::Monitor => not_yet("monitor", "monitor"),
        Command::Ai => not_yet("ai", "AI tools"),
    }
}

fn not_yet(command: &str, change: &str) -> CmdResult {
    Err(
        Failure::new(format!("`proxlet {command}` is not available yet"))
            .hint(format!(
                "It arrives with the {change} change; see openspec/changes."
            ))
            .code(2),
    )
}
