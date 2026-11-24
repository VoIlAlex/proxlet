# Design

## Context

See proposal.md for why. The current state this design builds on:

- **Request path** (`crates/proxlet-server/src/lib.rs`): `Engine::handle_from`
  answers ACME challenges, then redirects, then looks up the rule and calls
  `proxy::forward` or `files::serve`. Responses are
  `BoxBody<Bytes, BoxError>`, streamed, never buffered. `Origin::Front` trusts
  nginx's `X-Forwarded-For`.
- **Server messages:** about 30 `tracing` calls in `crates/proxlet` (daemon,
  ports, certs, watch) go through one `tracing_subscriber::fmt` subscriber to
  stderr (`init_logging` in `lib.rs`), which systemd sends to the journal.
- **Following a file:** `crate::watch::LogTail` follows nginx's error log by
  reopening the path on every poll and restarting when the file shrinks.
- **Settings:** a `Kind` per value type (`Bool`, `Port`, `Ip`, `Duration`,
  `AcmeServer`). Durations accept `ms`, `s` and `m`. Live settings reach the
  engine through the snapshot.

## Goals / Non-Goals

**Goals:**

- No blocking, locking or I/O on the request path: building a record is a few
  field copies and one non-blocking channel send.
- One writer, one file format and one ordering for both kinds of record, so
  `proxlet logs` (and later the monitor) reads one stream.
- Every limit applies live, through the existing settings path.

**Non-Goals:**

- Indexing or a query language: selectors are three simple filters, and
  anything more is `--json | jq`.
- Compressing rotated files.
- Recording at debug level, or the server's dependencies' messages.

## Decisions

### 1. Record format: one JSON object per line, two kinds

```json
{"t":"2026-11-24T01:06:12.040Z","kind":"request","client":"203.0.113.7","method":"GET","host":"example.com","path":"/api/users","status":200,"bytes":512,"ms":12.4,"rule":"/api","target":"127.0.0.1:8000"}
{"t":"2026-11-24T01:06:13.501Z","kind":"request","client":"203.0.113.7","method":"GET","host":"example.com","path":"/api","status":502,"bytes":96,"ms":0.8,"rule":"/api","target":"127.0.0.1:8000","error":"connection refused"}
{"t":"2026-11-24T01:06:20.002Z","kind":"event","level":"warn","msg":"port 443 is already in use by caddy (pid 812); waiting for it to be free"}
```

- **`t`:** UTC, millisecond precision, RFC 3339. It is formatted by a small
  helper; no new dependency.
- **`host`:** lowercase, without the port.
- **`path`:** without the query.
- **`target`:** the upstream `host:port` or the served directory. `rule` and
  `target` are absent when no rule matched.
- **Other answers:** a challenge or redirect answer is recorded with
  `rule: null` and `target` set to `acme-challenge` or `redirect`.

The field names are short because a file holds hundreds of thousands of lines.
They are documented in `docs/usage.md` as stable. *Alternative:* the
access-log text formats of nginx or Apache. Rejected: parsing them back for
selectors and `--json` is fragile, and nothing here needs compatibility with
them.

### 2. Access records are emitted when the body ends

The engine gains an optional sink, set by the daemon:
`Engine::set_access_log(Arc<dyn AccessLog>)`, where `AccessLog::record` takes
an `AccessRecord`. `handle_from` notes the start time and request facts, picks
the rule and target, and wraps the response body in a `Counted` body. That body
adds up the bytes of every data frame and calls the sink once, from `Drop`, so
the record covers:

- a full response;
- a client that went away mid-stream (bytes so far);
- an upgrade (`101`, bytes 0; the tunnelled bytes are not counted).

For a proxy error, `proxy::forward` already builds the 502/504 body from the
cause. The same reason text is passed out to fill `error`.

The client address is the peer address, or for `Origin::Front` the first
`X-Forwarded-For` entry, since nginx's own address carries no information.

`log.access` is read from the snapshot per request, so turning it off applies
to the next request.

### 3. Event records come from a `tracing` layer

`init_logging` becomes a registry with two layers: the existing `fmt` layer to
stderr, unchanged, and a `RecordLayer` that formats each event's message field
(`Visit`) for targets starting with `proxlet` at info level or above, and
sends it to the same queue. The roughly 30 existing call sites need no change.
Only `proxlet run` installs this layer; other commands log nothing to files.

### 4. One writer task, bounded queue, drop and count

There is one queue: `tokio::sync::mpsc` with capacity 8192, carrying record
structs that are serialized in the writer, not on the request path. Senders
use `try_send`. On `Full`, they add 1 to an `AtomicU64` drop counter. The
writer task:

- writes with a `BufWriter`, flushing at least every 200 ms, so a follower
  sees records within a second;
- after writing, records a warning event if the drop counter is non-zero
  (and resets it);
- on a write error, counts the records it couldn't write, and prints one
  warning to stderr with `eprintln!` (never `tracing`, which would feed back
  into itself), repeating at most once a minute. It retries opening the file
  every 5 seconds.

Shutdown closes the queue, and the writer drains and flushes before the daemon
exits.

### 5. Files, rotation and retention

- **Files:** `<state>/logs/proxlet.log` is the current file. Rotated files
  are `proxlet-<unix-millis>.log`, a name that sorts in time order. The
  directory is `0700`, and files are `0600`.
- **Rotation:** when the current file would pass 10 MB, the writer renames it
  to its rotated name and opens a new `proxlet.log`. A record is never split,
  because rotation happens between records.
- **Retention:** runs at startup, after each rotation, and hourly (for age).
  Rotated files are listed oldest first, and deleted while the total size
  (current file included) exceeds `log.max_size` or a file's modification time
  is older than `log.max_age`. The current file is never deleted, and
  `log.max_size` is at least 10 MB, which is one file.

The file size limit is a constant, 10 MB, not a setting. A test hook,
`PROXLET_TEST_LOG_FILE_BYTES`, lowers it.

### 6. `proxlet logs` reads the files, not the daemon

The command never talks to the server: it reads `<state>/logs/`, so it works
when proxlet is stopped, and a crashed server's last records are still there.

- **`--all`:** reads the rotated files oldest first, then the current file,
  printing the matching records.
- **Default:** finds the last 20 matching records by scanning files newest
  first. It reads whole files; at the default 100 MB that's a fraction of a
  second, which is acceptable for a development tool. Then it follows.
- **Following:** a new `Follower`, next to `LogTail` in `watch.rs`, keeps the
  current file open. Every 200 ms it reads what was appended. When
  `proxlet.log` has a different inode than the open file, rotation happened:
  it reads the old file to its end through the open handle, then opens the new
  one at offset 0. So nothing is lost or repeated. A missing file means
  waiting for it to appear.
- **Stopping:** Ctrl-C exits zero.

*Alternative:* streaming records from the daemon over the control socket.
Rejected: it doesn't work when the server is stopped, and it adds a long-lived
connection type to the control protocol.

### 7. Selectors and output

The selector is parsed once:

- **Port:** all digits, so `8000`, matching the `target` port.
- **Route:** starts with `/` or contains `/`, parsed with `Route::from_str`
  and compared to `rule` exactly.
- **Domain:** anything else, lowercased, compared to `host`.

An invalid route or domain exits non-zero with the parse error. Events are
shown only without a selector.

A terminal line looks like this:

```text
01:06:12.040  200  GET   example.com/api/users        12ms   512 B  → 127.0.0.1:8000
01:06:13.501  502  GET   example.com/api               1ms    96 B  → 127.0.0.1:8000  connection refused
01:06:20.002  WARN       port 443 is already in use by caddy (pid 812); waiting for it to be free
```

The date is added when it is not today. Columns are padded to fixed widths,
which keeps lines aligned while following without buffering. Status is green
for 2xx and 3xx, yellow for 4xx and red for 5xx, and levels are coloured the
same way. Piped output is the same text without colour, through `anstream`, as
other commands do. `--json` prints each matching stored line byte for byte.

Completion offers rule routes, hosts of host rules, and upstream ports, read
from `proxlet.toml` like the other completers.

### 8. Settings

- **Durations:** gain `h` and `d`. Formatting prefers the largest exact unit,
  so `7d` and not `168h`.
- **New `Kind::Size`:** `KB`, `MB` and `GB`, powers of 1024, shown in the
  largest exact unit.
- **New settings:** `log.access` (bool), `log.max_size` (size, minimum 10 MB,
  checked like the port rules) and `log.max_age` (duration). All are live: the
  writer reads them from the engine's snapshot at each retention run.

## Risks / Trade-offs

- **[Risk] Disk usage on a busy server.** About 200 bytes per request, so
  100 MB holds about half a million requests. → The size limit is a setting,
  and `log.access off` exists for heavy load tests.
- **[Risk] Client IP addresses are personal data.** → Files are `0600` in
  proxlet's own directory, kept for 7 days by default, and never include
  headers, queries or bodies. This is documented.
- **[Trade-off] Records for very long responses or WebSockets appear only when
  they end.** → That is when bytes and duration are known; events and other
  requests are not held up meanwhile.
- **[Trade-off] Dropping records under overload.** → Correctness of serving
  wins over completeness of logs. Drops are counted and recorded, and 8192
  slots absorb bursts.
- **[Risk] A follower on a network filesystem may miss inode changes.** → The
  state directory is local. A file that shrank is also treated as a rotation.

## Migration Plan

- There is nothing to migrate: the first `proxlet run` after upgrading
  creates `<state>/logs/`. Old servers' stderr output is unchanged.
- Rollback: delete `<state>/logs/`, or set `log.access off` to keep only event
  records.
