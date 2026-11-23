# Design

## Context

The repository is a cargo workspace with a CLI skeleton (`crates/proxlet`) and
five empty library crates. Motivation and scope are in `proposal.md`; behaviour
is in `specs/`. This document fixes the technical choices that the later changes
(logs, nginx attach, certificates, monitor, AI tools) will build on, so they are
made with those changes in mind even where this change does not use them yet.

## Goals / Non-Goals

**Goals:**

- A request path with no locks, no blocking calls and no per-request config reads.
- One code path for "validate, persist, apply" that every later mutation reuses.
- Crate boundaries that keep the service manager behind a trait from day one.
- A control channel that later changes can extend (log streaming, stats, attach).

**Non-Goals:**

- TLS, HTTP/3, and inbound HTTP/2 over TLS (certificates change).
- A stable on-disk or control-protocol format before 1.0.
- A launchd service manager; macOS uses `proxlet run` until someone adds one.

## Decisions

### 1. Engine: tokio + hyper 1.x, not Pingora or axum

hyper gives direct control over streaming, upgrades and connection pooling with
a small dependency tree, and works identically on Linux and macOS. Pingora is
heavier, Linux-first and brings its own process model, which fights the
daemon/CLI split. axum adds a router we do not need: matching is our own
longest-prefix table. Upstream connections use hyper-util's pooled client.

### 2. Rule table: immutable snapshot swapped atomically

The engine holds an `ArcSwap<RuleTable>`. Each request loads the current
snapshot once (a pointer read) and keeps it for its lifetime, so a reload never
affects in-flight requests and the hot path takes no lock. The table is a map
from host to a path-sorted list plus one hostless list; lookup is a hash probe
and a short scan from longest prefix to shortest. Settings live in the same
snapshot, which is how `static.listing` changes apply live.

*Alternative:* `RwLock<RuleTable>` — simpler, but puts a lock on every request
and lets a slow writer stall traffic.

### 3. One TOML file, edited in place

Rules and settings live in one `proxlet.toml`, edited with `toml_edit` so
hand-written comments and ordering survive CLI edits. Settings sit in one table
per key group (`[listen]`, `[static]`, `[proxy]`), so the dotted key
`static.listing` is literally `listing` in `[static]`. Each rule is a `[[rule]]`
with `route` and exactly one of `proxy` (a URL) or `files` (an absolute path).
A setting changed back to its default is removed from the file.
Writes go to a temp file in the same directory followed by `rename`, under an
advisory file lock so two CLI invocations cannot interleave.

*Alternative:* separate `config.toml` and `rules.toml` — rejected: two files to
keep consistent and no benefit to the user.

### 4. Validate → apply → persist, with the daemon as the applier

A mutation runs in this order:

1. The CLI parses and validates the input into a typed change (pure, in `proxlet-core`).
2. If the daemon is running, the CLI sends the change over the control socket.
   The daemon builds the new snapshot, and only if that succeeds writes the file
   and swaps the snapshot. The daemon is the only writer while it runs.
3. If the daemon is not running, the CLI builds the snapshot itself (same code)
   and writes the file.

Because the snapshot is built before anything is written, a failure leaves both
the file and the running server untouched; there is nothing to roll back. Later
changes that must touch external state (nginx config, certificates) add a step
between "build" and "persist" with its own test-and-rollback.

### 5. Control channel: line-delimited JSON over a Unix socket

Each request is one JSON object on one line; responses are one or more lines, so
streaming replies (logs, stats) need no new transport later. The socket's
presence and a successful `ping` are what "running" means for `status` and for a
second `proxlet run`.

*Alternative:* HTTP on a loopback port — rejected: any local process or browser
page could reach it, and it needs a port.

### 6. The daemon runs as the invoking user, not root

`proxlet start` needs root once, to write the systemd unit. The unit runs the
daemon as the user who invoked `sudo proxlet start` (`SUDO_USER`), with
`AmbientCapabilities=CAP_NET_BIND_SERVICE` so it can bind port 80. Consequences:

- `proxlet add /files .` can read the user's own files without a root daemon
  serving arbitrary paths.
- The config file, socket and state live in that user's directories
  (`$XDG_CONFIG_HOME/proxlet` and `$XDG_STATE_HOME/proxlet`), so `add` / `rm` /
  `ls` / `config` never need sudo. The unit passes both directories to the
  daemon explicitly (`PROXLET_CONFIG_DIR`, `PROXLET_STATE_DIR`), so CLI and
  daemon cannot disagree.
- The socket lives in the state directory, not `$XDG_RUNTIME_DIR`: a system
  service does not get the user's runtime directory, and the CLI and the daemon
  must agree on the path. If the path exceeds the ~100-byte Unix socket limit,
  it moves to `/tmp/proxlet-<uid>/<hash>.sock`, a 0700 directory that the daemon
  refuses to use if another user owns it.
- When `proxlet start` is run directly as root, the daemon runs as root and uses
  `/etc/proxlet` and `/var/lib/proxlet`.

`PROXLET_HOME` overrides all locations; tests and CI rely on it.

*Alternative:* a root daemon with a group-writable socket — rejected: anyone in
the group could make root serve any directory.

### 7. Service managers behind a trait

`proxlet-service` exposes a `ServiceManager` trait (`detect`, `install`,
`start`, `stop`, `disable`, `status`) and a `detect()` function returning the
first manager that reports itself present. systemd is the only implementation.
It shells out to `systemctl` rather than speaking D-Bus: no extra dependencies,
and the commands are what a user would run by hand when debugging. When the unit
fails to start, the implementation surfaces the server's own last error lines
(from the journal) so the user sees "port 80 is held by apache2", not "unit
failed".

### 8. Port check: try to bind, then explain

The server simply binds; on `EADDRINUSE` it looks up the holder to produce the
error message (`/proc` on Linux, `lsof` on macOS, best effort). Checking first
and binding second would be racy. The lookup sits behind a small function in
`proxlet-core` that the nginx attach change will reuse to recognise nginx.

### 9. Static files: resolve, canonicalise, compare

A request path is percent-decoded, rejected if any segment is `..` or contains a
separator or NUL, joined to the root, canonicalised, and served only if the
result is still under the canonicalised root. That one check covers traversal and
symlink escapes. Dot-segments are checked on the decoded path before touching the
filesystem. File bodies are streamed; conditional requests use mtime and size.

### 10. Proxy header policy: keep the client's Host

The spec keeps the original `Host`, unlike nginx's default of sending the
upstream's. Dev servers and frameworks build absolute URLs and check allowed
hosts against what the browser sent, so preserving it is the less surprising
default. A per-rule override is a later, additive option.

### 11. Completions come from the config file

Dynamic completion (rule routes, setting keys and values) reads `proxlet.toml`
directly instead of asking the daemon: it must answer in a few milliseconds and
work when the daemon is stopped.

## Risks / Trade-offs

- [Running as the invoking user means a second user on the same host cannot manage
  the same proxlet] → Acceptable for the dev-first use case; a system-wide mode
  exists by running `start` as root.
- [`SUDO_USER` is absent under some sudo configurations or with `su`] → Fall back
  to root mode and say so in the `start` output.
- [Shelling out to `systemctl` ties us to its output and exit codes] → Use only
  documented exit codes and `--property` queries, never parse human output.
- [Finding the process on a busy port is unreliable without privileges] → The
  error always names the port; the process name is best effort.
- [`toml_edit` round-tripping cannot recover a file the user broke by hand] → On a
  parse error every command reports the line and refuses to write, so a broken
  file is never overwritten.
- [macOS has no service manager implementation] → `start` fails with a pointer to
  `proxlet run`; the trait makes launchd a self-contained follow-up.

### 12. Implementation notes

- **Minimum Rust 1.89**, for `std::fs::File::lock` (the config-file lock) and
  let-chains, instead of adding a locking crate.
- **`https://` upstreams** use rustls with the Mozilla root store (webpki-roots),
  so they behave the same on every host.
- **Readiness.** `start` waits for the daemon to answer on its control socket,
  rather than trusting the service manager's "active" state. A crash-and-retry
  (`Restarting`) or `Failed` state ends the wait early. The daemon's own
  `error:` line and hints are then lifted from the journal, and the service is
  stopped so it doesn't retry in a loop.
- **`stop`** also stops a foreground `proxlet run`, through the control socket.

## Open Questions

- Whether `proxlet ls` should show upstream reachability, or leave that to
  `monitor`. The shutdown grace period is a fixed 10s for now; make it a setting
  if anyone needs it.
