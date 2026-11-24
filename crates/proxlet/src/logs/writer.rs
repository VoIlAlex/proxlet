//! The one log writer: a bounded queue that never blocks a sender, and a
//! thread that writes, rotates and enforces retention (design decisions 4
//! and 5).

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use proxlet_server::{AccessLog, AccessRecord};

use super::files::{self, Limits};
use super::record::Line;

/// How many records may wait for the writer.
pub const QUEUE: usize = 8192;
/// How often buffered records reach the file.
pub const FLUSH_EVERY: Duration = Duration::from_millis(200);
/// How often a broken file is opened again.
pub const RETRY_EVERY: Duration = Duration::from_secs(5);
/// How often a write failure is reported on stderr.
pub const WARN_EVERY: Duration = Duration::from_secs(60);
/// How often old files are looked for without a rotation.
pub const RETENTION_EVERY: Duration = Duration::from_secs(3600);

/// What goes through the queue. Serializing happens in the writer.
#[derive(Debug)]
pub enum Entry {
    /// An access record.
    Request(AccessRecord),
    /// A server message.
    Event {
        /// When it happened.
        time: SystemTime,
        /// `info`, `warn` or `error`.
        level: &'static str,
        /// The text.
        msg: String,
    },
    /// Write what is queued and stop.
    Close,
}

/// The sending side, cheap to clone. Sending never blocks: when the queue is
/// full the record is dropped and counted.
#[derive(Debug, Clone)]
pub struct LogSender {
    tx: SyncSender<Entry>,
    dropped: Arc<AtomicU64>,
}

impl LogSender {
    /// Queues an entry, or counts it as dropped.
    pub fn send(&self, entry: Entry) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(entry) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl AccessLog for LogSender {
    fn record(&self, record: AccessRecord) {
        self.send(Entry::Request(record));
    }
}

/// The receiving side, until the writer starts.
#[derive(Debug)]
pub struct Queue {
    rx: Receiver<Entry>,
    tx: SyncSender<Entry>,
    dropped: Arc<AtomicU64>,
}

/// A new queue of [`QUEUE`] entries.
#[must_use]
pub fn queue() -> (LogSender, Queue) {
    queue_of(QUEUE)
}

fn queue_of(capacity: usize) -> (LogSender, Queue) {
    let (tx, rx) = sync_channel(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        LogSender {
            tx: tx.clone(),
            dropped: dropped.clone(),
        },
        Queue { rx, tx, dropped },
    )
}

/// Reads the retention limits; called at every retention run so changes
/// apply live.
pub type LimitsFn = Arc<dyn Fn() -> Limits + Send + Sync>;

/// The running writer. Dropping it writes what is queued and stops it.
#[derive(Debug)]
pub struct WriterHandle {
    tx: SyncSender<Entry>,
    thread: Option<JoinHandle<()>>,
}

impl WriterHandle {
    /// Writes what is queued, flushes and stops the writer.
    pub fn close(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.tx.send(Entry::Close);
            let _ = thread.join();
        }
    }
}

impl Drop for WriterHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts writing the queue's records to `dir`.
///
/// # Errors
///
/// When the thread cannot be started.
pub fn start(dir: PathBuf, queue: Queue, limits: LimitsFn) -> io::Result<WriterHandle> {
    start_with(dir, queue, limits, files::file_limit())
}

fn start_with(
    dir: PathBuf,
    queue: Queue,
    limits: LimitsFn,
    file_limit: u64,
) -> io::Result<WriterHandle> {
    let Queue { rx, tx, dropped } = queue;
    let writer = Writer {
        dir,
        file: None,
        size: 0,
        buffered: 0,
        file_limit,
        limits,
        dropped,
        lost: 0,
        last_warn: None,
        last_open: None,
        last_retention: Instant::now(),
    };
    let thread = std::thread::Builder::new()
        .name("proxlet-log".into())
        .spawn(move || writer.run(&rx))?;
    Ok(WriterHandle {
        tx,
        thread: Some(thread),
    })
}

struct Writer {
    dir: PathBuf,
    file: Option<BufWriter<File>>,
    /// Bytes in the current file, including what is buffered.
    size: u64,
    /// Records written since the last flush, lost if the flush fails.
    buffered: u64,
    file_limit: u64,
    limits: LimitsFn,
    /// Records the queue had no room for.
    dropped: Arc<AtomicU64>,
    /// Records that could not be written.
    lost: u64,
    last_warn: Option<Instant>,
    last_open: Option<Instant>,
    last_retention: Instant,
}

impl Writer {
    fn run(mut self, rx: &Receiver<Entry>) {
        self.open();
        self.retention();
        let mut last_flush = Instant::now();
        loop {
            match rx.recv_timeout(FLUSH_EVERY) {
                Ok(Entry::Close) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(entry) => self.write_entry(entry),
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_flush.elapsed() >= FLUSH_EVERY {
                self.tick();
                last_flush = Instant::now();
            }
        }
        while let Ok(entry) = rx.try_recv() {
            if !matches!(entry, Entry::Close) {
                self.write_entry(entry);
            }
        }
        self.tick();
    }

    fn write_entry(&mut self, entry: Entry) {
        let line = match entry {
            Entry::Request(r) => Line::request(r),
            Entry::Event { time, level, msg } => Line::event(time, level, msg),
            Entry::Close => return,
        };
        let mut json = line.to_json();
        json.push('\n');
        if !self.write_line(json.as_bytes()) {
            self.lost += 1;
        }
    }

    /// Writes one whole line, rotating first if it would pass the limit.
    fn write_line(&mut self, line: &[u8]) -> bool {
        if self.file.is_none() && !self.reopen() {
            return false;
        }
        let len = line.len() as u64;
        if self.size > 0 && self.size + len > self.file_limit {
            self.rotate();
        }
        let Some(file) = self.file.as_mut() else {
            return false;
        };
        match file.write_all(line) {
            Ok(()) => {
                self.size += len;
                self.buffered += 1;
                true
            }
            Err(e) => {
                self.fail(&e);
                false
            }
        }
    }

    /// Flushes, records drops, and runs the hourly retention.
    fn tick(&mut self) {
        if let Some(file) = self.file.as_mut() {
            if let Err(e) = file.flush() {
                self.lost += self.buffered;
                self.fail(&e);
            }
            self.buffered = 0;
        }
        if self.file.is_none() && !self.reopen() {
            return;
        }
        let dropped = self.dropped.swap(0, Ordering::Relaxed) + self.lost;
        if dropped > 0 {
            self.lost = 0;
            let msg = format!(
                "{dropped} log record{} could not be written (the queue was full or writing failed)",
                if dropped == 1 { "" } else { "s" }
            );
            let line = Line::event(SystemTime::now(), "warn", msg);
            let mut json = line.to_json();
            json.push('\n');
            if self.write_line(json.as_bytes()) {
                if let Some(Err(e)) = self.file.as_mut().map(Write::flush) {
                    self.fail(&e);
                }
                self.buffered = 0;
            } else {
                self.lost += dropped;
            }
        }
        if self.last_retention.elapsed() >= RETENTION_EVERY {
            self.retention();
        }
    }

    fn open(&mut self) -> bool {
        self.last_open = Some(Instant::now());
        let opened = files::create_dir(&self.dir).and_then(|()| {
            let f = files::open_current(&self.dir)?;
            let size = f.metadata()?.len();
            Ok((f, size))
        });
        match opened {
            Ok((f, size)) => {
                self.file = Some(BufWriter::with_capacity(64 * 1024, f));
                self.size = size;
                true
            }
            Err(e) => {
                self.fail(&e);
                false
            }
        }
    }

    fn reopen(&mut self) -> bool {
        if self.last_open.is_some_and(|t| t.elapsed() < RETRY_EVERY) {
            return false;
        }
        self.open()
    }

    fn rotate(&mut self) {
        if let Some(mut file) = self.file.take()
            && let Err(e) = file.flush()
        {
            self.lost += self.buffered;
            self.fail(&e);
            return;
        }
        self.buffered = 0;
        if let Err(e) = files::rotate(&self.dir, SystemTime::now()) {
            self.fail(&e);
            return;
        }
        self.open();
        self.retention();
    }

    fn retention(&mut self) {
        self.last_retention = Instant::now();
        if let Err(e) = files::retention(
            &self.dir,
            (self.limits)(),
            self.file_limit,
            SystemTime::now(),
        ) {
            self.warn(&format!(
                "cannot delete old log files in {}: {e}",
                self.dir.display()
            ));
        }
    }

    /// The file is unusable until the next successful open.
    fn fail(&mut self, e: &io::Error) {
        self.file = None;
        self.buffered = 0;
        self.warn(&format!(
            "cannot write the log in {}: {e}; records are dropped until it can",
            self.dir.display()
        ));
    }

    /// Prints to stderr, at most once every [`WARN_EVERY`]. Never through
    /// `tracing`, which would feed the warning back into this writer.
    fn warn(&mut self, msg: &str) {
        if self.last_warn.is_some_and(|t| t.elapsed() < WARN_EVERY) {
            return;
        }
        self.last_warn = Some(Instant::now());
        eprintln!("proxlet: {msg}");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use proxlet_core::settings::MB;

    use super::*;

    fn limits(max_size: u64) -> LimitsFn {
        Arc::new(move || Limits {
            max_size,
            max_age: Duration::from_secs(86_400),
        })
    }

    fn event(msg: &str) -> Entry {
        Entry::Event {
            time: SystemTime::now(),
            level: "info",
            msg: msg.into(),
        }
    }

    fn lines(dir: &Path) -> Vec<Line> {
        files::all(dir)
            .unwrap()
            .iter()
            .flat_map(|p| {
                fs::read_to_string(p)
                    .unwrap()
                    .lines()
                    .map(|l| serde_json::from_str::<Line>(l).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn msgs(dir: &Path) -> Vec<String> {
        lines(dir).into_iter().filter_map(|l| l.msg).collect()
    }

    #[test]
    fn writes_in_order_and_flushes_while_running() {
        let d = tempfile::tempdir().unwrap();
        let (tx, q) = queue();
        let w = start(d.path().to_owned(), q, limits(100 * MB)).unwrap();
        for i in 0..100 {
            tx.send(event(&i.to_string()));
        }
        // Visible without closing, within the flush interval.
        let deadline = Instant::now() + Duration::from_secs(2);
        while msgs(d.path()).len() < 100 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let expected: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        assert_eq!(msgs(d.path()), expected);
        w.close();
    }

    #[test]
    fn close_drains_the_queue() {
        let d = tempfile::tempdir().unwrap();
        let (tx, q) = queue();
        let w = start(d.path().to_owned(), q, limits(100 * MB)).unwrap();
        for i in 0..1000 {
            tx.send(event(&i.to_string()));
        }
        w.close();
        assert_eq!(msgs(d.path()).len(), 1000);
        // Sending after the writer stopped is harmless.
        tx.send(event("late"));
    }

    #[test]
    fn full_queue_drops_and_records_how_many() {
        let d = tempfile::tempdir().unwrap();
        let (tx, q) = queue_of(10);
        for i in 0..25 {
            tx.send(event(&i.to_string()));
        }
        let w = start(d.path().to_owned(), q, limits(100 * MB)).unwrap();
        w.close();
        let got = msgs(d.path());
        assert_eq!(got.len(), 11, "{got:?}");
        assert_eq!(got[9], "9");
        assert!(
            got[10].starts_with("15 log records could not be written"),
            "{}",
            got[10]
        );
    }

    #[test]
    fn unwritable_directory_never_blocks_senders() {
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
        if fs::write(locked.join("probe"), "").is_ok() {
            return; // running as root: permissions are not enforced
        }
        let (tx, q) = queue_of(16);
        let w = start(locked.join("logs"), q, limits(100 * MB)).unwrap();
        let started = Instant::now();
        for i in 0..10_000 {
            tx.send(event(&i.to_string()));
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        w.close();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn rotation_keeps_records_whole_and_within_the_size_limit() {
        let d = tempfile::tempdir().unwrap();
        let (tx, q) = queue();
        let w = start_with(d.path().to_owned(), q, limits(30 * 1024), 10 * 1024).unwrap();
        // About 45 KB of records.
        for i in 0..450 {
            tx.send(event(&format!("{i:04} {}", "x".repeat(40))));
        }
        w.close();
        let all = files::all(d.path()).unwrap();
        let total: u64 = all.iter().map(|p| fs::metadata(p).unwrap().len()).sum();
        assert!(total <= 30 * 1024, "{total}");
        assert!(all.len() >= 3);
        for p in &all {
            assert!(fs::metadata(p).unwrap().len() <= 10 * 1024);
        }
        // Every line parses (none split) and the newest is kept.
        let got = msgs(d.path());
        assert!(got.last().unwrap().starts_with("0449 "));
        let first: usize = got[0][..4].parse().unwrap();
        let expected: Vec<usize> = (first..450).collect();
        let seen: Vec<usize> = got.iter().map(|m| m[..4].parse().unwrap()).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn limits_apply_live_at_the_next_rotation() {
        let d = tempfile::tempdir().unwrap();
        let max = Arc::new(AtomicU64::new(100 * 1024));
        let read = max.clone();
        let limits: LimitsFn = Arc::new(move || Limits {
            max_size: read.load(Ordering::Relaxed),
            max_age: Duration::from_secs(86_400),
        });
        let (tx, q) = queue();
        let w = start_with(d.path().to_owned(), q, limits, 10 * 1024).unwrap();
        let fill = |from: usize| {
            for i in from..from + 450 {
                tx.send(event(&format!("{i:05} {}", "x".repeat(40))));
            }
        };
        fill(0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while files::all(d.path()).unwrap().len() < 5 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(files::all(d.path()).unwrap().len() >= 5);
        max.store(20 * 1024, Ordering::Relaxed);
        fill(450);
        w.close();
        let total: u64 = files::all(d.path())
            .unwrap()
            .iter()
            .map(|p| fs::metadata(p).unwrap().len())
            .sum();
        assert!(total <= 20 * 1024, "{total}");
    }
}
