//! proxlet — a tiny, fast reverse proxy and HTTP server.
//!
//! The binary is both the CLI and the daemon. This library holds everything
//! behind `main` so integration tests can drive it.

pub mod attach;
pub mod certs;
pub mod cli;
pub mod commands;
pub mod complete;
pub mod control;
pub mod daemon;
pub mod logs;
pub mod mcp;
pub mod monitor;
pub mod ops;
pub mod output;
pub mod pipeline;
pub mod ports;
pub mod watch;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use proxlet_core::Locations;

use crate::cli::{AiAction, CertAction, Cli, Command};
use crate::output::CmdResult;

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

/// Messages go to stderr, filtered by `PROXLET_LOG`; with `record`, proxlet's
/// own info-and-above messages also become event records.
fn init_logging(record: Option<logs::layer::RecordLayer>) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, Layer, filter};
    let env = EnvFilter::try_from_env("PROXLET_LOG")
        .unwrap_or_else(|_| EnvFilter::new("proxlet=info,proxlet_server=info"));
    let fmt = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr);
    // The journal timestamps lines itself.
    let fmt = if std::env::var_os(daemon::SERVICE_ENV).is_some() {
        fmt.without_time().boxed()
    } else {
        fmt.boxed()
    };
    let record = record.map(|r| r.with_filter(filter::filter_fn(logs::layer::RecordLayer::wants)));
    tracing_subscriber::registry()
        .with(fmt.with_filter(env))
        .with(record)
        .init();
}

fn dispatch(command: &Command, locations: &Locations) -> CmdResult {
    match command {
        Command::Run => {
            let (sender, queue) = logs::writer::queue();
            init_logging(Some(logs::layer::RecordLayer::new(sender.clone())));
            daemon::run(locations, sender, queue)
        }
        Command::Start { yes } => commands::service::start(locations, *yes),
        Command::Stop => commands::service::stop(locations),
        Command::Status => commands::status::run(locations),
        Command::Add { route, target, yes } => commands::rules::add(locations, route, target, *yes),
        Command::Rm { route } => commands::rules::rm(locations, route),
        Command::Ls => commands::rules::ls(locations),
        Command::Config { key, value } => {
            commands::config::run(locations, key.as_deref(), value.as_deref())
        }
        Command::Handoff { action, socket } => {
            commands::handoff::run(locations, *action, socket.as_deref())
        }
        Command::Logs {
            selector,
            all,
            json,
        } => commands::logs::run(locations, selector.as_deref(), *all, *json),
        Command::Cert {
            action,
            domains,
            yes,
        } => match action {
            None => commands::cert::issue(locations, domains, *yes),
            Some(CertAction::Ls) => commands::cert::ls(locations),
            Some(CertAction::Rm { domains }) => commands::cert::rm(locations, domains),
            Some(CertAction::Renew { domains }) => commands::cert::renew(locations, domains),
        },
        Command::Monitor { once } => commands::monitor::run(locations, *once),
        Command::Ai { action, tools, yes } => match action {
            None => commands::ai::connect(tools, *yes),
            Some(AiAction::Rm { tools }) => commands::ai::rm(tools),
        },
        Command::Mcp => commands::mcp::run(locations),
    }
}
