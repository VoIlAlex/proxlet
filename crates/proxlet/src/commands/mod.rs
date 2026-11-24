//! Command implementations.

pub mod cert;
pub mod config;
pub mod handoff;
pub mod rules;
pub mod service;
pub mod status;

use proxlet_core::{Change, ConfigFile, Locations};

use crate::control::{Client, Request, WireChange, WireOutcome};
use crate::output::{self, Failure};
use crate::pipeline;

/// The result of submitting a change.
#[derive(Debug)]
pub struct Submitted {
    /// What the change did.
    pub outcome: WireOutcome,
    /// Whether a running server applied it (false: only the file was written).
    pub live: bool,
}

/// Applies a change through the running daemon, or directly to the file when
/// proxlet is not running. Either way the change is validated first and
/// nothing is written if it is rejected.
pub fn submit(locations: &Locations, change: &Change) -> Result<Submitted, Failure> {
    if let Ok(mut client) = Client::connect(&locations.socket()) {
        let reply = client
            .call(&Request::Apply {
                change: WireChange::from_change(change),
            })
            .map_err(|e| Failure::new(format!("proxlet is running but did not answer: {e}")))?;
        if !reply.ok {
            return Err(Failure::new(
                reply
                    .error
                    .unwrap_or_else(|| "proxlet rejected the change".into()),
            ));
        }
        let outcome = reply
            .outcome
            .ok_or_else(|| Failure::new("proxlet applied the change but sent no outcome"))?;
        return Ok(Submitted {
            outcome,
            live: true,
        });
    }
    let file = ConfigFile::new(locations.config_file());
    let (outcome, _) = pipeline::apply(&file, change).map_err(|e| Failure::new(e.to_string()))?;
    Ok(Submitted {
        outcome: WireOutcome::from(&outcome),
        live: false,
    })
}

/// Tells the user a change was saved but nothing is serving it yet.
pub fn note_not_running() {
    output::note("proxlet is not running; this applies when it starts (proxlet start).");
}
