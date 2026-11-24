//! Live traffic statistics per rule (design decisions 2 and 3).
//!
//! Recording is a handful of relaxed atomic operations on fixed-size
//! structures: no lock, no allocation. Windows are kept in a ring of 60
//! one-second slots; a slot is claimed for a new second with a
//! compare-exchange and zeroed by the thread that won. A request landing on a
//! second boundary while the slot is being zeroed may be lost; the numbers are
//! for a live display.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const RELAXED: Ordering = Ordering::Relaxed;

/// Seconds kept in the ring.
const SLOTS: u64 = 60;
/// The rate window: the last 10 complete seconds.
const RATE_WINDOW: u64 = 10;
/// Sub-buckets per power of two.
const SUB: u32 = 8;
/// Below 2^6 µs (64 µs) everything is in bucket 0.
const MIN_EXP: u32 = 6;
/// From 2^31 µs (~36 minutes) up, everything is in the last bucket.
const MAX_EXP: u32 = 31;
/// Number of latency buckets.
const BUCKETS: usize = 1 + ((MAX_EXP - MIN_EXP + 1) * SUB) as usize;
/// A slot that has never been used.
const UNUSED: u64 = u64::MAX;

/// Seconds since the first use of the statistics in this process.
fn now_second() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs()
}

/// The bucket for a latency in microseconds.
fn bucket(us: u64) -> usize {
    if us < 1 << MIN_EXP {
        return 0;
    }
    let msb = us.ilog2();
    if msb > MAX_EXP {
        return BUCKETS - 1;
    }
    let sub = u32::try_from((us >> (msb - 3)) & u64::from(SUB - 1)).unwrap_or(0);
    (1 + (msb - MIN_EXP) * SUB + sub) as usize
}

/// The midpoint of a bucket, in microseconds.
fn midpoint(index: usize) -> u64 {
    if index == 0 {
        return 1 << (MIN_EXP - 1);
    }
    let i = u32::try_from(index - 1).unwrap_or(0);
    let exp = i / SUB + MIN_EXP;
    let sub = u64::from(i % SUB);
    let width = 1u64 << (exp - 3);
    (u64::from(SUB) + sub) * width + width / 2
}

struct Slot {
    second: AtomicU64,
    requests: AtomicU64,
    bytes: AtomicU64,
    latency: [AtomicU32; BUCKETS],
}

impl Slot {
    fn new() -> Self {
        Self {
            second: AtomicU64::new(UNUSED),
            requests: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            latency: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    /// Makes this slot the one for `now`, zeroing it if it held an older
    /// second. False if it already holds a later second.
    fn claim(&self, now: u64) -> bool {
        let held = self.second.load(Ordering::Acquire);
        if held == now {
            return true;
        }
        if held != UNUSED && held > now {
            return false;
        }
        if self
            .second
            .compare_exchange(held, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.requests.store(0, RELAXED);
            self.bytes.store(0, RELAXED);
            for b in &self.latency {
                b.store(0, RELAXED);
            }
        }
        true
    }

    fn holds(&self, from: u64, to: u64) -> bool {
        let s = self.second.load(Ordering::Acquire);
        s != UNUSED && s >= from && s <= to
    }
}

/// The statistics of one rule (or of the "no rule" row).
pub struct RuleStats {
    total: AtomicU64,
    status_4xx: AtomicU64,
    status_5xx: AtomicU64,
    upstream_errors: AtomicU64,
    in_flight: AtomicU32,
    websockets: AtomicU32,
    slots: Box<[Slot]>,
}

impl std::fmt::Debug for RuleStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleStats")
            .field("total", &self.total.load(RELAXED))
            .finish_non_exhaustive()
    }
}

impl Default for RuleStats {
    fn default() -> Self {
        Self {
            total: AtomicU64::new(0),
            status_4xx: AtomicU64::new(0),
            status_5xx: AtomicU64::new(0),
            upstream_errors: AtomicU64::new(0),
            in_flight: AtomicU32::new(0),
            websockets: AtomicU32::new(0),
            slots: (0..SLOTS).map(|_| Slot::new()).collect(),
        }
    }
}

/// What one rule did, read at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Totals {
    /// Requests per second over the last 10 complete seconds.
    pub rps: f64,
    /// Requests since the server started (or the rule was added).
    pub total: u64,
    /// 4xx responses.
    pub status_4xx: u64,
    /// 5xx responses.
    pub status_5xx: u64,
    /// 502/504 answers from proxlet failing to reach the upstream.
    pub upstream_errors: u64,
    /// Median latency over the last 60 seconds; `None` without requests.
    pub p50: Option<Duration>,
    /// 95th percentile latency over the last 60 seconds.
    pub p95: Option<Duration>,
    /// 99th percentile latency over the last 60 seconds.
    pub p99: Option<Duration>,
    /// Bytes sent per second over the last 10 complete seconds.
    pub out_bps: f64,
    /// Requests whose response has not ended.
    pub in_flight: u32,
    /// Open WebSocket tunnels.
    pub websockets: u32,
}

impl RuleStats {
    fn slot(&self, now: u64) -> &Slot {
        &self.slots[usize::try_from(now % SLOTS).unwrap_or(0)]
    }

    /// Records a response head: its status, whether proxlet failed to reach
    /// the upstream, and the time from arrival.
    pub(crate) fn record(&self, status: u16, upstream_error: bool, latency: Duration) {
        self.record_at(now_second(), status, upstream_error, latency);
    }

    fn record_at(&self, now: u64, status: u16, upstream_error: bool, latency: Duration) {
        self.total.fetch_add(1, RELAXED);
        match status {
            400..=499 => {
                self.status_4xx.fetch_add(1, RELAXED);
            }
            500..=599 => {
                self.status_5xx.fetch_add(1, RELAXED);
            }
            _ => {}
        }
        if upstream_error {
            self.upstream_errors.fetch_add(1, RELAXED);
        }
        let slot = self.slot(now);
        if slot.claim(now) {
            slot.requests.fetch_add(1, RELAXED);
            let us = u64::try_from(latency.as_micros()).unwrap_or(u64::MAX);
            slot.latency[bucket(us)].fetch_add(1, RELAXED);
        }
    }

    /// Records body bytes sent now.
    pub(crate) fn record_bytes(&self, n: u64) {
        self.record_bytes_at(now_second(), n);
    }

    fn record_bytes_at(&self, now: u64, n: u64) {
        let slot = self.slot(now);
        if slot.claim(now) {
            slot.bytes.fetch_add(n, RELAXED);
        }
    }

    /// Reads the statistics now.
    #[must_use]
    pub fn read(&self) -> Totals {
        self.read_at(now_second())
    }

    fn read_at(&self, now: u64) -> Totals {
        let rate_from = now.saturating_sub(RATE_WINDOW);
        // `now` itself is incomplete; with `now == 0` the range is empty.
        let rate_seconds = now - rate_from;
        let latency_from = now.saturating_sub(SLOTS - 1);
        let mut requests = 0u64;
        let mut bytes = 0u64;
        let mut hist = [0u64; BUCKETS];
        for slot in &self.slots {
            if rate_seconds > 0 && slot.holds(rate_from, now - 1) {
                requests += slot.requests.load(RELAXED);
                bytes += slot.bytes.load(RELAXED);
            }
            if slot.holds(latency_from, now) {
                for (h, b) in hist.iter_mut().zip(&slot.latency) {
                    *h += u64::from(b.load(RELAXED));
                }
            }
        }
        let per_second = |n: u64| {
            if rate_seconds == 0 {
                0.0
            } else {
                #[allow(clippy::cast_precision_loss)]
                let r = n as f64 / rate_seconds as f64;
                r
            }
        };
        Totals {
            rps: per_second(requests),
            total: self.total.load(RELAXED),
            status_4xx: self.status_4xx.load(RELAXED),
            status_5xx: self.status_5xx.load(RELAXED),
            upstream_errors: self.upstream_errors.load(RELAXED),
            p50: percentile(&hist, 50),
            p95: percentile(&hist, 95),
            p99: percentile(&hist, 99),
            out_bps: per_second(bytes),
            in_flight: self.in_flight.load(RELAXED),
            websockets: self.websockets.load(RELAXED),
        }
    }
}

/// The midpoint of the bucket holding the `pct`-th percentile.
fn percentile(hist: &[u64; BUCKETS], pct: u64) -> Option<Duration> {
    let count: u64 = hist.iter().sum();
    if count == 0 {
        return None;
    }
    let rank = (count * pct).div_ceil(100).max(1);
    let mut seen = 0;
    for (i, n) in hist.iter().enumerate() {
        seen += n;
        if seen >= rank {
            return Some(Duration::from_micros(midpoint(i)));
        }
    }
    None
}

/// Counts one request in flight until dropped.
#[derive(Debug)]
pub(crate) struct InFlight(pub(crate) Arc<RuleStats>);

impl InFlight {
    pub(crate) fn start(stats: Arc<RuleStats>) -> Self {
        stats.in_flight.fetch_add(1, RELAXED);
        Self(stats)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, RELAXED);
    }
}

/// Counts one open WebSocket tunnel until dropped.
pub(crate) struct Tunnel(Arc<RuleStats>);

impl Tunnel {
    pub(crate) fn open(stats: Arc<RuleStats>) -> Self {
        stats.websockets.fetch_add(1, RELAXED);
        Self(stats)
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.0.websockets.fetch_sub(1, RELAXED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn buckets_are_ordered_and_midpoints_close() {
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(63), 0);
        assert_eq!(bucket(64), 1);
        assert_eq!(bucket(u64::MAX), BUCKETS - 1);
        assert_eq!(bucket((1 << 32) - 1), BUCKETS - 1);
        let mut last = 0;
        for us in (64..10_000_000u64).step_by(997) {
            let b = bucket(us);
            assert!(b >= last, "{us}");
            last = b;
            #[allow(clippy::cast_precision_loss)]
            let err = (midpoint(b) as f64 - us as f64).abs() / us as f64;
            assert!(err <= 0.0625 + 1e-9, "{us}µs → {} ({err})", midpoint(b));
        }
    }

    #[test]
    fn rates_cover_the_last_ten_complete_seconds() {
        let s = RuleStats::default();
        for sec in 100..110 {
            for _ in 0..3 {
                s.record_at(sec, 200, false, ms(1));
            }
            s.record_bytes_at(sec, 1000);
        }
        // The current second is not counted in rates.
        s.record_at(110, 200, false, ms(1));
        let t = s.read_at(110);
        assert!((t.rps - 3.0).abs() < 1e-9, "{}", t.rps);
        assert!((t.out_bps - 1000.0).abs() < 1e-9);
        assert_eq!(t.total, 31);
    }

    #[test]
    fn old_slots_are_ignored_after_a_gap() {
        let s = RuleStats::default();
        s.record_at(10, 200, false, ms(5));
        s.record_bytes_at(10, 50);
        let t = s.read_at(200);
        assert!(t.rps.abs() < f64::EPSILON);
        assert!(t.out_bps.abs() < f64::EPSILON);
        assert_eq!(t.p50, None);
        assert_eq!(t.total, 1);
        // Slot 10 is reused for second 70 and starts from zero.
        s.record_at(70, 200, false, ms(5));
        let t = s.read_at(71);
        assert!((t.rps - 0.1).abs() < 1e-9, "{}", t.rps);
    }

    #[test]
    fn no_traffic_has_no_latency() {
        let t = RuleStats::default().read_at(5);
        assert_eq!((t.p50, t.p95, t.p99), (None, None, None));
        assert_eq!(t.total, 0);
        assert!(RuleStats::default().read_at(0).rps.abs() < f64::EPSILON);
    }

    #[test]
    fn percentiles_and_status_classes() {
        let s = RuleStats::default();
        for i in 0..100 {
            let latency = if i < 90 { ms(10) } else { ms(200) };
            s.record_at(50, 200, false, latency);
        }
        s.record_at(50, 404, false, ms(1));
        s.record_at(50, 502, true, ms(1));
        s.record_at(50, 500, false, ms(1));
        let t = s.read_at(50);
        let near = |d: Option<Duration>, want: f64| {
            let got = d.unwrap().as_secs_f64() * 1000.0;
            assert!((got - want).abs() / want <= 0.0625, "{got} vs {want}");
        };
        near(t.p50, 10.0);
        near(t.p95, 200.0);
        near(t.p99, 200.0);
        assert_eq!((t.status_4xx, t.status_5xx, t.upstream_errors), (1, 2, 1));
        assert_eq!(t.total, 103);
    }

    #[test]
    fn gauges_follow_guards() {
        let s = Arc::new(RuleStats::default());
        let a = InFlight::start(s.clone());
        let w = Tunnel::open(s.clone());
        assert_eq!((s.read().in_flight, s.read().websockets), (1, 1));
        drop(a);
        drop(w);
        assert_eq!((s.read().in_flight, s.read().websockets), (0, 0));
    }

    #[test]
    fn concurrent_recording_counts_every_request() {
        let s = Arc::new(RuleStats::default());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for i in 0..50_000u64 {
                        s.record_at(7, 200, false, Duration::from_micros(i % 5000));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let t = s.read_at(7);
        assert_eq!(t.total, 400_000);
        assert!(t.p99.is_some());
    }
}
