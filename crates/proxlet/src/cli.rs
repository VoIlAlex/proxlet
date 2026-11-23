//! The command-line interface definition.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCompleter, PathCompleter};

use crate::complete;

/// A tiny, fast reverse proxy and HTTP server.
#[derive(Debug, Parser)]
#[command(name = "proxlet", version, about, propagate_version = true)]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Every proxlet command.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install and start the proxlet system service
    Start,
    /// Stop the proxlet system service
    Stop,
    /// Run the server in the foreground (what the system service executes)
    Run,
    /// Show whether proxlet is running and how
    Status,
    /// Add or update a rule: `proxlet add [HOST]/PATH TARGET`
    #[command(
        after_help = "Examples:\n  proxlet add /api 8000\n  proxlet add example.com/api 10.0.0.5:9000\n  proxlet add /files ."
    )]
    Add {
        /// Where requests come in: `/api` or `example.com/api`
        #[arg(add = ArgValueCompleter::new(complete::rule_routes))]
        route: String,
        /// Where they go: a local port (`8000`), `host:port`, a URL, or a directory (`.`)
        #[arg(add = ArgValueCompleter::new(PathCompleter::dir()))]
        target: String,
    },
    /// Remove a rule
    Rm {
        /// The rule to remove: `/api` or `example.com/api`
        #[arg(add = ArgValueCompleter::new(complete::rule_routes))]
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
    /// Show or change settings: `proxlet config [KEY [VALUE]]`
    Config {
        /// Setting name, e.g. `static.listing`; omit to list all settings
        #[arg(add = ArgValueCompleter::new(complete::setting_keys))]
        key: Option<String>,
        /// New value; omit to print the current one
        #[arg(add = ArgValueCompleter::new(complete::setting_values))]
        value: Option<String>,
    },
    /// Hand the HTTP port to a front server, or take it back (used by service hooks)
    #[command(hide = true)]
    Handoff {
        /// `release` before the front server starts, `reclaim` after it stops
        action: HandoffAction,
        /// The daemon's control socket, when running as another user
        #[arg(long)]
        socket: Option<PathBuf>,
    },
}

/// What `proxlet handoff` asks the daemon to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HandoffAction {
    /// Let go of the HTTP port.
    Release,
    /// Take the HTTP port back.
    Reclaim,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

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
