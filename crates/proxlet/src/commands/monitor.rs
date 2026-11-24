//! `proxlet monitor`.

use std::time::{Duration, Instant};

use proxlet_core::Locations;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};

use crate::monitor::view::{Action, View};
use crate::monitor::{Daemon, Fetch, format, tui};
use crate::output::{self, CmdResult, Failure};

/// How often the table is refreshed.
const TICK: Duration = Duration::from_secs(1);

/// `proxlet monitor [--once]`.
pub fn run(locations: &Locations, once: bool) -> CmdResult {
    let mut daemon = Daemon::new(locations.socket());
    let first = daemon.fetch().map_err(failure)?;
    if once || !output::stdout_is_terminal() {
        print!("{}", format::plain(&first));
        return Ok(());
    }
    let mut view = View::default();
    view.update(Some(first));
    let mut terminal = ratatui::init();
    let result = live(&mut terminal, &mut daemon, &mut view);
    ratatui::restore();
    result.map_err(|e| Failure::new(format!("the terminal failed: {e}")))
}

pub(crate) fn failure(why: Fetch) -> Failure {
    match why {
        Fetch::NotRunning => {
            Failure::new("proxlet is not running").hint("Start it with: proxlet start")
        }
        Fetch::TooOld => Failure::new("the running proxlet server is too old for monitor")
            .hint("Restart it to run the installed version: proxlet start"),
        Fetch::Failed(e) => Failure::new(format!("proxlet could not report statistics: {e}")),
    }
}

/// Redraws every second, and after each key, until a quit key.
fn live(
    terminal: &mut DefaultTerminal,
    daemon: &mut Daemon,
    view: &mut View,
) -> std::io::Result<()> {
    let mut next = Instant::now() + TICK;
    loop {
        terminal.draw(|f| tui::draw(f, view))?;
        let wait = next.saturating_duration_since(Instant::now());
        if event::poll(wait)? {
            if let Event::Key(key) = event::read()?
                && view.key(key, tui::visible_rows(terminal.size()?.height)) == Action::Quit
            {
                return Ok(());
            }
            continue;
        }
        view.update(daemon.fetch().ok());
        next = Instant::now() + TICK;
    }
}
