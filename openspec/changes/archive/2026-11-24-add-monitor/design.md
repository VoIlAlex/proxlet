# Design

## Context

The engine (`proxlet-server`) already observes every answered request.
`Engine::handle_from` works out which rule answered, a `Served`. When access
logging is on, it wraps the response body in `Counted`, which counts the bytes
sent and emits an `AccessRecord` when the body is dropped. Proxy failures carry
an `UpstreamError` response extension, and WebSocket tunnels run in a task
spawned by `proxy::forward`.

The snapshot (`Snapshot`) is immutable and is rebuilt from scratch on every
rule or setting change, then swapped in by `Engine::replace` through
`ArcSwap`. The CLI talks to the daemon over the line-delimited JSON control
socket (`control.rs`), with one `Request` and one `Reply` per line.

## Goals / Non-Goals

**Goals:**
- Per-request recording costs a few relaxed atomic operations, with no locks,
  no allocation and no hash lookup.
- Statistics survive snapshot swaps for rules whose route is unchanged.
- The TUI and plain renderers share one formatting path. Both can be tested
  without a real terminal.

**Non-Goals:**
- Exact figures under concurrent races at a second boundary. The numbers are
  for a live display, and an occasional miscount of one request is acceptable.
- Sharing code with `proxlet logs`. The monitor reads the daemon, never the log
  files.

## Decisions

### 1. Each rule's statistics live in its snapshot entry, carried across swaps

`ProxyTarget` and `FilesTarget` each gain `stats: Arc<RuleStats>`, and
`Snapshot` gains `unmatched: Arc<RuleStats>` for the "no rule" row.
`Snapshot::build` creates fresh ones. `Engine::replace` then takes the
statistics over from the old snapshot: an entry whose route text is unchanged
gets the old `Arc`, and so does `unmatched`. A target change keeps the totals,
because only the route identifies the row. A removed route's statistics are
dropped with the old snapshot, so adding the route again starts from zero.

The request path already holds the matched entry, so finding the statistics
is a field access. The alternatives were:
- a `HashMap<route, Arc<RuleStats>>` behind `ArcSwap`, which costs a hash per
  request;
- a `DashMap`, which takes locks and adds a dependency.

A request served by an old snapshot after a swap records into an `Arc` that is
being dropped. Losing that count is fine.

### 2. A ring of 60 one-second slots per rule, plus lifetime totals

`RuleStats` holds two kinds of data:
- **Totals:** `AtomicU64` counts of requests, 4xx, 5xx and upstream errors, and
  `AtomicU32` gauges for requests in flight and open WebSockets.
- **A ring of 60 slots,** each with `second: AtomicU64` (seconds since the
  engine started), requests, bytes sent, and a latency histogram.

To record, a request computes `now`, picks slot `now % 60` and checks its
`second`. If the value is old, the request tries a `compare_exchange` to `now`.
The winner zeroes the counters, and every request then adds with `Relaxed`.

Readers sum only the slots whose `second` is in range:
- for rates, the last 10 *complete* seconds, `[now-10, now-1]`, divided by 10;
- for latency, `[now-59, now]`.

The alternatives were:
- an exponentially decayed rate, which is cheap but hard to state as "last 10
  s";
- `hdrhistogram` behind a `Mutex`, which takes a lock and allocates.

### 3. A fixed log-linear latency histogram

There are 8 sub-buckets per power of two of microseconds, from 64 µs (bucket
0 holds everything below that) up to about 2^32 µs (bucket overflow holds
everything above). That makes 209 `AtomicU32` buckets per slot, with a relative
error of at most about 9%. A percentile is the midpoint of the bucket where the
running count crosses the rank.

Memory is 60 × 209 × 4 B ≈ 50 KB per rule, which is fixed. The test scenario
allows 180–260 ms for an upstream that takes 200 ms, which fits the error.

The alternative of 4 sub-buckets per power of two halves the memory, but its
error of about 19% is too coarse to tell 200 ms from 240 ms.

### 4. What is recorded, and when

`handle_from` gets the `Served` value and the response head from `respond`,
and then:
- **Latency and counts** are recorded at that point, measured from arrival
  until the head is ready. This includes the requests total, the slot's
  requests, the status class, and an upstream error when the response carries
  the `UpstreamError` extension. The extension is read before access logging
  takes it.
- **Bytes sent** are added to the slot as each frame goes out. The body
  wrapper is now always applied, not only when access logging is on.
  - `Counted` becomes a wrapper with an optional `Pending` for the access
    record, plus the rule's statistics.
  - That costs one boxed body per response even with logging off, which is
    acceptable next to a proxied hop.
- **In flight** is incremented when the request arrives and decremented when
  the body wrapper is dropped. For an early answer, the decrement is still in
  `Drop`, so the gauge cannot leak.
- **WebSockets:** `proxy::forward` gets the rule's statistics. The tunnel task
  increments `websockets` once both upgrades succeed. A guard decrements it
  when `copy_bidirectional` returns or the task is dropped.

Challenge answers, redirects and no-rule answers record into
`snapshot.unmatched`.

### 5. Open connections

`Engine` gains `connections: AtomicUsize`. `serve_io` holds a guard that
increments it while the connection is served. TLS connections are counted
from after the handshake. The count goes into the snapshot header rather than
into rows, because a connection carries requests for many rules.

### 6. Control protocol

`Request::Stats` is answered with `Reply.stats: Option<StatsReply>`:

```json
{"ok":true,"stats":{"uptime_secs":120,"connections":3,"rows":[
  {"rule":"/api","target":"127.0.0.1:8000","rps":4.2,"total":512,
   "status_4xx":3,"status_5xx":1,"upstream_errors":1,
   "p50_ms":12.4,"p95_ms":40.1,"p99_ms":88.0,"out_bps":20480.0,
   "in_flight":1,"websockets":0},
  {"rule":null,"target":null,"rps":0.0,"total":2,...}]}}
```

- Latency fields are `null` with no requests in the window.
- Rows are in `proxlet ls` order, and the "no rule" row is last.
- The engine produces a plain `StatsSnapshot` struct, so `proxlet-server`
  needs no serde. `control.rs` maps it onto the wire type.
- The AI tools change can reuse the same request.

### 7. `proxlet monitor`: one loop, no async runtime

The command runs synchronously:
- `control::Client` connects once and sends `stats` each second.
- `crossterm::event::poll(timeout)` waits for keys until the next tick.
- On a socket error, the view switches to "proxlet is not running —
  reconnecting…", and the command tries to connect each second.

The code is split in three parts:
- `monitor::view`: pure state, holding the latest snapshot, sort column,
  scroll offset and connection state, plus key handling.
- `monitor::format`: cell text, shared with the plain output.
  - Rates look like `4.2`.
  - Durations look like `12ms`, `1.2s` and `-`.
  - Bytes per second look like `20KB/s` and `1.5MB/s`.
- `monitor::tui`: draws a `ratatui::widgets::Table`.

The terminal is set up with `ratatui::init()`, which installs a panic hook
that restores the terminal, and is put back with `ratatui::restore()` on every
exit path.

When stdout is not a terminal (`std::io::IsTerminal`), or with `--once`, the
command sends one `stats` request and prints aligned plain columns:

```text
RULE           TARGET          REQ/S  TOTAL  4XX  5XX  UP-ERR   P50   P95   P99   OUT/S  ACTIVE  WS
/api           127.0.0.1:8000    4.2    512    3    1       1  12ms  40ms  88ms  20KB/s       1   0
(no rule)      -                 0.0      2    2    0       0     -     -     -   0B/s        0   0
```

It does not wait 10 seconds first: the rates come from the daemon's window,
so a snapshot is meaningful at once.

The alternative of tokio with `crossterm`'s `event-stream` adds features and
complexity for a loop that ticks once a second.

### 8. Sort and scroll

`s` cycles through four sorts:
- route (ascending, the default);
- req/s (descending);
- errors, which is 4xx + 5xx (descending);
- p95 (descending, with `-` last).

The current sort column's header gets a `▼`. The "no rule" row always stays
last. `↑`/`↓` and `PgUp`/`PgDn` scroll when there are more rows than fit on the
screen.

## Risks / Trade-offs

- **Slot reset race:** a request that lands at a second boundary can add to a
  slot just before another request zeroes it. → The miscount is at most a few
  requests at a boundary. This is stated as a non-goal, and the tests allow
  for it by spacing requests or checking ranges.
- **Clock:** `Instant` is used for seconds since engine start, so wall-clock
  jumps cannot corrupt the windows.
- **Always wrapping bodies:** with logging off, each response costs one more
  allocation than today. → The spec forbids allocation per request in the
  statistics *recording*. The body wrapper is one allocation per response,
  which is already the case with logging on, the default.
- **Terminal left broken on a crash:** the panic hook from `ratatui::init()`
  and restoring on every return path cover it. A `kill -9` cannot be covered,
  as with any TUI.
- **Old daemon, new CLI:** an older daemon answers `stats` with an "unknown
  command" error. → The monitor shows "this proxlet server is too old for
  monitor; restart it (proxlet start)" and exits 1. The daemon and the CLI are
  the same binary, so this only happens across an upgrade without a restart.

## Migration Plan

There is no persistent state to migrate. Installing the new binary and
restarting the service enables the statistics. Rolling back is reinstalling
the previous binary.
