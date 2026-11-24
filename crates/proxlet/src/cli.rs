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
    Start {
        /// Make proxlet nginx's default server without asking
        #[arg(long, short)]
        yes: bool,
    },
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
        /// Add it even if nginx already serves the host, without asking
        #[arg(long, short)]
        yes: bool,
    },
    /// Remove a rule
    Rm {
        /// The rule to remove: `/api` or `example.com/api`
        #[arg(add = ArgValueCompleter::new(complete::rule_routes))]
        route: String,
    },
    /// List all rules
    Ls,
    /// Show the last requests and server events, then follow new ones
    #[command(
        after_help = "Examples:\n  proxlet logs\n  proxlet logs /api\n  proxlet logs example.com --all\n  proxlet logs 8000 --all --json"
    )]
    Logs {
        /// A route (`/api`), a domain (`example.com`) or an upstream port
        /// (`8000`); omit for every request and server event
        #[arg(add = ArgValueCompleter::new(complete::log_selectors))]
        selector: Option<String>,
        /// Print every stored record, oldest first, and exit
        #[arg(long)]
        all: bool,
        /// Print the stored JSON records instead of aligned lines
        #[arg(long)]
        json: bool,
    },
    /// Get Let's Encrypt certificates: `proxlet cert <domain>…`, or `ls`, `rm`, `renew`
    #[command(
        args_conflicts_with_subcommands = true,
        after_help = "Examples:\n  proxlet cert example.com\n  proxlet cert ls\n  proxlet cert renew example.com\n  proxlet cert rm example.com"
    )]
    Cert {
        /// What to do instead of getting certificates.
        #[command(subcommand)]
        action: Option<CertAction>,
        /// Domains to get a certificate for; omit to list certificates
        #[arg(add = ArgValueCompleter::new(complete::rule_hosts))]
        domains: Vec<String>,
        /// Accept the certificate authority's terms of service without asking
        #[arg(long, short)]
        yes: bool,
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

/// `proxlet cert` subcommands.
#[derive(Debug, Subcommand)]
pub enum CertAction {
    /// List certificates with their expiry
    Ls,
    /// Delete certificates
    Rm {
        /// The domains whose certificates to delete
        #[arg(required = true, add = ArgValueCompleter::new(complete::cert_domains))]
        domains: Vec<String>,
    },
    /// Renew certificates now, even if they are not due (all when none given)
    Renew {
        /// The domains to renew; omit for every certificate
        #[arg(add = ArgValueCompleter::new(complete::cert_domains))]
        domains: Vec<String>,
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
    fn cert_parses_domains_and_subcommands() {
        let parse = |args: &[&str]| {
            let mut all = vec!["proxlet", "cert"];
            all.extend_from_slice(args);
            match Cli::try_parse_from(all).unwrap().command {
                Command::Cert {
                    action,
                    domains,
                    yes,
                } => (action, domains, yes),
                other => panic!("expected cert, got {other:?}"),
            }
        };
        let (action, domains, yes) = parse(&["a.example.com", "b.example.com", "--yes"]);
        assert!(action.is_none() && yes);
        assert_eq!(domains, ["a.example.com", "b.example.com"]);
        assert!(matches!(parse(&["ls"]).0, Some(CertAction::Ls)));
        assert!(matches!(
            parse(&["rm", "a.example.com"]).0,
            Some(CertAction::Rm { domains }) if domains == ["a.example.com"]
        ));
        assert!(matches!(
            parse(&["renew"]).0,
            Some(CertAction::Renew { domains }) if domains.is_empty()
        ));
        let (action, domains, _) = parse(&[]);
        assert!(action.is_none() && domains.is_empty());
        // `rm` needs a domain.
        assert!(Cli::try_parse_from(["proxlet", "cert", "rm"]).is_err());
    }

    #[test]
    fn add_parses_route_and_target() {
        let cli = Cli::try_parse_from(["proxlet", "add", "example.com/api", "8002"]).unwrap();
        match cli.command {
            Command::Add { route, target, .. } => {
                assert_eq!(route, "example.com/api");
                assert_eq!(target, "8002");
            }
            other => panic!("expected add, got {other:?}"),
        }
    }
}
