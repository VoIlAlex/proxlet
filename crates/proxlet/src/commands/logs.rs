//! `proxlet logs [<selector>] [--all] [--json]`: reads the log files, never
//! the server, so it works whether or not proxlet is running.

use std::io::{self, Write};
use std::time::Duration;

use proxlet_core::Locations;

use crate::logs::files;
use crate::logs::read::{self, Follower};
use crate::logs::record::Line;
use crate::logs::show::{self, Clock, Selector};
use crate::output::{self, CmdResult, Failure};

/// How many records are shown before following.
pub const TAIL: usize = 20;
/// How often the current file is read while following.
pub const FOLLOW_EVERY: Duration = Duration::from_millis(200);

struct Printer {
    selector: Option<Selector>,
    json: bool,
    clock: Clock,
    out: anstream::AutoStream<io::Stdout>,
}

impl Printer {
    fn parse(&self, raw: &str) -> Option<Line> {
        let line: Line = serde_json::from_str(raw).ok()?;
        show::shown(self.selector.as_ref(), &line).then_some(line)
    }

    fn wants(&self, raw: &str) -> bool {
        self.parse(raw).is_some()
    }

    /// Prints a stored line if it is selected. `Err` when stdout is gone
    /// (`proxlet logs | head`).
    fn print(&mut self, raw: &str) -> io::Result<bool> {
        let Some(line) = self.parse(raw) else {
            return Ok(false);
        };
        if self.json {
            writeln!(self.out, "{raw}")?;
        } else {
            writeln!(self.out, "{}", show::format(&line, self.clock))?;
        }
        Ok(true)
    }
}

/// Prints stored records, then follows new ones unless `all` is set.
pub fn run(locations: &Locations, selector: Option<&str>, all: bool, json: bool) -> CmdResult {
    let selector = crate::ops::selector(selector)?;
    // While the process is single-threaded, so the local offset can be read.
    let clock = Clock::local();
    let dir = files::dir(locations);
    let mut printer = Printer {
        selector,
        json,
        clock,
        out: anstream::stdout(),
    };
    let unreadable =
        |e: io::Error| Failure::new(format!("cannot read the logs in {}: {e}", dir.display()));

    if all {
        let mut shown = 0usize;
        let mut gone = false;
        read::each_line(&dir, |raw| {
            if !gone {
                match printer.print(raw) {
                    Ok(true) => shown += 1,
                    Ok(false) => {}
                    Err(_) => gone = true,
                }
            }
        })
        .map_err(unreadable)?;
        if shown == 0 && !gone {
            output::note(nothing_yet(printer.selector.is_some()));
        }
        return Ok(());
    }

    let mut follower = Follower::new(&dir);
    let current = follower.poll().map_err(unreadable)?;
    let history =
        read::last_matching(&dir, current, TAIL, |raw| printer.wants(raw)).map_err(unreadable)?;
    if history.is_empty() {
        output::note(format!(
            "{}; waiting for new ones (Ctrl-C to stop)",
            nothing_yet(printer.selector.is_some())
        ));
    }
    for raw in &history {
        if printer.print(raw).is_err() {
            return Ok(());
        }
    }
    follow(&mut printer, &mut follower).map_err(unreadable)
}

fn nothing_yet(selected: bool) -> &'static str {
    if selected {
        "No matching log records"
    } else {
        "No log records yet"
    }
}

/// Prints new records until Ctrl-C (exit zero) or until stdout closes.
fn follow(printer: &mut Printer, follower: &mut Follower) -> io::Result<()> {
    // One thread: nothing here needs more, and it keeps the local-offset
    // lookup valid.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        let mut tick = tokio::time::interval(FOLLOW_EVERY);
        loop {
            tokio::select! {
                _ = &mut ctrl_c => return Ok(()),
                _ = tick.tick() => {
                    for raw in follower.poll()? {
                        if printer.print(&raw).is_err() {
                            return Ok(());
                        }
                    }
                    if printer.out.flush().is_err() {
                        return Ok(());
                    }
                }
            }
        }
    })
}
