//! `proxlet monitor`: live statistics per rule, read from the daemon.

pub mod format;
pub mod tui;
pub mod view;

use std::path::PathBuf;
use std::time::Duration;

use crate::control::{Client, Request, StatsReply};

/// Why the statistics could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    /// Nothing answered on the control socket.
    NotRunning,
    /// The running server predates `stats`.
    TooOld,
    /// The server answered with an error.
    Failed(String),
}

/// Reads statistics from the daemon, keeping one control connection open and
/// reconnecting when it breaks.
#[derive(Debug)]
pub struct Daemon {
    socket: PathBuf,
    client: Option<Client>,
}

impl Daemon {
    /// A reader for the daemon listening on `socket`.
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            client: None,
        }
    }

    /// Asks for the statistics once.
    pub fn fetch(&mut self) -> Result<StatsReply, Fetch> {
        let client = match &mut self.client {
            Some(c) => c,
            None => self.client.insert(
                Client::connect_with_timeout(&self.socket, Duration::from_secs(2))
                    .map_err(|_| Fetch::NotRunning)?,
            ),
        };
        let Ok(reply) = client.call(&Request::Stats) else {
            self.client = None;
            return Err(Fetch::NotRunning);
        };
        if reply.ok {
            return reply
                .stats
                .ok_or_else(|| Fetch::Failed("the reply had no statistics".into()));
        }
        let error = reply.error.unwrap_or_default();
        if error.starts_with("malformed request") {
            Err(Fetch::TooOld)
        } else {
            Err(Fetch::Failed(error))
        }
    }
}
