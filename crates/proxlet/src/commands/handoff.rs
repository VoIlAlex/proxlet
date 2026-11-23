//! `proxlet handoff release|reclaim`: called by service hooks around a front
//! server's start and stop.
//!
//! It must never keep the front server from starting, so it always succeeds:
//! problems are reported on stderr (which lands in the front server's journal)
//! and the exit code stays 0.

use std::path::Path;
use std::time::Duration;

use proxlet_core::Locations;

use crate::cli::HandoffAction;
use crate::control::{Client, Request};
use crate::output::CmdResult;

/// How long to wait for the daemon to confirm.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// Sends the handoff request and reports what happened.
pub fn run(locations: &Locations, action: HandoffAction, socket: Option<&Path>) -> CmdResult {
    let default = locations.socket();
    let socket = socket.unwrap_or(&default);
    let (request, done) = match action {
        HandoffAction::Release => (Request::Release, "released the HTTP port"),
        HandoffAction::Reclaim => (Request::Reclaim, "asked to take the HTTP port back"),
    };
    let reply = Client::connect_with_timeout(socket, TIMEOUT).and_then(|mut c| c.call(&request));
    match reply {
        Ok(r) if r.ok => eprintln!("proxlet: {done}"),
        Ok(r) => eprintln!(
            "proxlet: {}",
            r.error.unwrap_or_else(|| "request refused".into())
        ),
        Err(_) => eprintln!("proxlet: not running; nothing to hand over"),
    }
    Ok(())
}
