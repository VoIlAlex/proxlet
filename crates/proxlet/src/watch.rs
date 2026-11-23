//! Noticing a front server that tries to start while proxlet holds the port
//! (design decision 7): follow its error log for bind failures, or, when the
//! log cannot be read, watch for its process to appear.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use proxlet_front::FrontServer;
use tokio::sync::{mpsc, watch};

use crate::ports::{Command, Mode};

/// How often the log is read.
pub const LOG_POLL: Duration = Duration::from_millis(100);
/// How often the process list is checked when the log cannot be read.
pub const PROCESS_POLL: Duration = Duration::from_millis(250);

/// Reads the lines appended to a file since the last call.
#[derive(Debug)]
pub struct LogTail {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl LogTail {
    /// Starts at the current end of `path` (or its start, once it appears).
    #[must_use]
    pub fn new(path: &Path) -> Self {
        let offset = std::fs::metadata(path).map_or(0, |m| m.len());
        Self {
            path: path.to_owned(),
            offset,
            partial: String::new(),
        }
    }

    /// Whether the log can be followed: it is readable, or it does not exist
    /// yet in a directory that is.
    #[must_use]
    pub fn readable(path: &Path) -> bool {
        match File::open(path) {
            Ok(_) => true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                path.parent().is_some_and(|d| std::fs::read_dir(d).is_ok())
            }
            Err(_) => false,
        }
    }

    /// Complete lines appended since the last call. A file that shrank (it
    /// was rotated or truncated) is read again from the start.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.offset = 0;
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.take(len - self.offset).read_to_end(&mut buf)?;
        self.offset = len;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = self.partial.split('\n').map(str::to_owned).collect();
        self.partial = lines.pop().unwrap_or_default();
        Ok(lines)
    }
}

/// Turns "is the process running" samples into "it just started".
#[derive(Debug)]
pub struct ProcessEdge {
    was_running: bool,
}

impl ProcessEdge {
    /// Starts from the current state, so a process already running (e.g. on
    /// other ports) never counts as starting.
    #[must_use]
    pub fn new(running_now: bool) -> Self {
        Self {
            was_running: running_now,
        }
    }

    /// Whether the process started since the last sample.
    pub fn started(&mut self, running: bool) -> bool {
        let started = running && !self.was_running;
        self.was_running = running;
        started
    }
}

/// Follows the front server while proxlet holds the port, and asks the port
/// owner to release it when the front server tries to bind.
pub async fn run(
    front: Arc<dyn FrontServer>,
    log: Option<PathBuf>,
    port: u16,
    attach_enabled: impl Fn() -> bool + Send + 'static,
    mode: watch::Receiver<Mode>,
    commands: mpsc::Sender<Command>,
    mut stop: watch::Receiver<bool>,
) {
    let readable_log = log.filter(|p| LogTail::readable(p));
    if let Some(p) = &readable_log {
        tracing::debug!("following {} for bind failures", p.display());
    } else {
        tracing::debug!(
            "cannot read {}'s error log; watching for its process instead",
            front.name()
        );
    }
    let mut tail = readable_log.as_deref().map(LogTail::new);
    let mut edge = {
        let front = front.clone();
        ProcessEdge::new(
            tokio::task::spawn_blocking(move || front.is_running())
                .await
                .unwrap_or(false),
        )
    };
    let interval = if tail.is_some() {
        LOG_POLL
    } else {
        PROCESS_POLL
    };
    loop {
        tokio::select! {
            _ = stop.wait_for(|s| *s) => return,
            () = tokio::time::sleep(interval) => {}
        }
        let standalone = *mode.borrow() == Mode::Standalone;
        let wants_port = if let Some(t) = &mut tail {
            let lines = t.poll().unwrap_or_default();
            lines.iter().any(|l| front.is_bind_failure(l, port))
        } else {
            let f = front.clone();
            let running = tokio::task::spawn_blocking(move || f.is_running())
                .await
                .unwrap_or(false);
            edge.started(running)
        };
        if wants_port && standalone && attach_enabled() {
            tracing::info!(
                "{} is trying to bind port {port}; handing it over",
                front.name()
            );
            let _ = commands.send(Command::Release(None)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn append(path: &Path, text: &str) {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }

    #[test]
    fn tail_reads_only_new_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("error.log");
        append(&log, "old line\n");
        let mut t = LogTail::new(&log);
        assert_eq!(t.poll().unwrap(), Vec::<String>::new());
        append(&log, "first\nsecond half");
        assert_eq!(t.poll().unwrap(), ["first"]);
        append(&log, "-done\n");
        assert_eq!(t.poll().unwrap(), ["second half-done"]);
    }

    #[test]
    fn tail_follows_rotation_and_a_late_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("error.log");
        let mut t = LogTail::new(&log);
        assert!(
            LogTail::readable(&log),
            "a missing file in a readable directory"
        );
        assert_eq!(t.poll().unwrap(), Vec::<String>::new());
        append(&log, "appeared\n");
        assert_eq!(t.poll().unwrap(), ["appeared"]);
        std::fs::write(&log, "new\n").unwrap();
        assert_eq!(t.poll().unwrap(), ["new"]);
    }

    #[test]
    fn unreadable_log_is_reported() {
        assert!(!LogTail::readable(Path::new("/nonexistent-dir/error.log")));
    }

    #[test]
    fn process_edge_fires_once_per_start() {
        let mut e = ProcessEdge::new(false);
        assert!(!e.started(false));
        assert!(e.started(true));
        assert!(!e.started(true));
        assert!(!e.started(false));
        assert!(e.started(true));
        // Already running when proxlet started: not a start.
        let mut e = ProcessEdge::new(true);
        assert!(!e.started(true));
    }
}
