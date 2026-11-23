//! proxlet — a tiny, fast reverse proxy and HTTP server.
//!
//! This binary is both the CLI and the daemon. The command surface below is the
//! agreed public interface; behaviour lands change by change (see `openspec/`).

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::CompleteEnv;

#[derive(Debug, Parser)]
#[command(name = "proxlet", version, about, propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install and start the proxlet system service
    Start,
    /// Stop the proxlet system service
    Stop,
    /// Run the server in the foreground (what the system service executes)
    Run,
    /// Show whether proxlet is running and whether it is standalone or attached
    Status,
    /// Add or update a rule: `proxlet add [HOST]/PATH TARGET`
    Add {
        /// Where requests come in: `/api` or `example.com/api`
        route: String,
        /// Where they go: a local port (`8000`), `host:port`, a URL, or a directory (`.`)
        target: String,
    },
    /// Remove a rule
    Rm {
        /// The rule to remove: `/api` or `example.com/api`
        route: String,
    },
    /// List all rules
    Ls,
    /// Follow logs, optionally filtered by route, domain or port
    Logs {
        /// `/api`, `example.com` or `8000`; omit for everything
        selector: Option<String>,
        /// Print all available logs and exit instead of following
        #[arg(long)]
        all: bool,
    },
    /// Obtain a Let's Encrypt certificate for a domain
    Cert {
        /// The domain to issue a certificate for
        domain: String,
    },
    /// Live table of rules with traffic stats
    Monitor,
    /// Connect proxlet to installed AI tools (registers its MCP server)
    Ai,
    /// Show or change proxlet settings
    Config {
        /// Setting name, e.g. `static.listing`; omit to list all settings
        key: Option<String>,
        /// New value; omit to print the current one
        value: Option<String>,
    },
}

fn main() -> ExitCode {
    // Serves shell completions when invoked by the shell (`COMPLETE=zsh proxlet`).
    CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();
    eprintln!(
        "proxlet: `{}` is not implemented yet — see openspec/changes for the plan",
        command_name(&cli.command)
    );
    ExitCode::from(2)
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Start => "start",
        Command::Stop => "stop",
        Command::Run => "run",
        Command::Status => "status",
        Command::Add { .. } => "add",
        Command::Rm { .. } => "rm",
        Command::Ls => "ls",
        Command::Logs { .. } => "logs",
        Command::Cert { .. } => "cert",
        Command::Monitor => "monitor",
        Command::Ai => "ai",
        Command::Config { .. } => "config",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn add_parses_route_and_target() {
        let cli = Cli::try_parse_from(["proxlet", "add", "example.com/api", "8002"]).unwrap();
        match cli.command {
            Command::Add { route, target } => {
                assert_eq!(route, "example.com/api");
                assert_eq!(target, "8002");
            }
            other => panic!("expected add, got {other:?}"),
        }
    }
}
