# Proposal

## Why

When a route misbehaves, there is no way to see what proxlet did with a
request: nothing records requests, and the server's own messages go to the
terminal or the journal, so they are gone after a `proxlet run` exits and
cannot be filtered by route. `proxlet logs` is the third command the README
promises after rules and certificates, and the planned monitor and AI tools
build on the same records.

## What Changes

- **Log files.** The running server writes one JSON object per line to
  `<state>/logs/`. It rotates at 10 MB and deletes the oldest files beyond
  `log.max_size` (100 MB) or `log.max_age` (7 days). Files are readable only by
  the user proxlet runs as. The server keeps printing its messages to stderr,
  and so to the journal under systemd, as today.
- **Access records.** Every request is recorded once its response has been
  sent: time, client address, method, host, path, status, bytes sent,
  duration, the matched rule, the upstream or directory, and the reason for a
  proxy error. Headers, query strings and bodies are never recorded.
  `log.access off` stops recording requests.
- **Event records.** The server's own messages, such as port changes,
  attaching, and certificate issuance and renewal, are recorded with their
  level.
- **Never on the request path.** Records go through a bounded queue to one
  writer. When the queue is full, records are dropped and counted, and the
  drop count is itself recorded. A full disk or a write error never slows or
  fails a request.
- **`proxlet logs [<selector>]`** prints the last 20 matching records and then
  follows new ones until interrupted. It works whether or not proxlet is
  running.
  - **Selectors:** a route (`/api`, `example.com/api`), a domain
    (`example.com`) or an upstream port (`8000`). The selector tab-completes.
  - **Flags:** `--all` prints every stored record and exits. `--json` prints
    the stored JSON lines.
  - **Output:** in a terminal, aligned coloured lines. When piped, the same
    lines without colour, so `grep` works.
- **New settings:** `log.access` (`on`, live), `log.max_size` (`100MB`, live),
  `log.max_age` (`7d`, live). Durations also accept `h` and `d`, and a new size
  kind accepts `KB`, `MB` and `GB`.

### Out of scope

- `proxlet monitor` (live rates, errors and latency per rule): the *monitor*
  change, which will read the same records.
- The MCP server and `proxlet ai`: the *AI tools* change.
- Shipping logs elsewhere (syslog, OpenTelemetry, a remote collector), and
  log formats other than proxlet's JSON lines: not planned for now.
- Recording headers, query strings or bodies: not planned, for privacy.
- Logging requests that nginx serves itself when attached: those never reach
  proxlet.

## Capabilities

### New Capabilities

- `logs`: what the server records about requests and about itself, where and
  for how long it keeps the records, and `proxlet logs` for reading,
  filtering and following them.

### Modified Capabilities

- `settings`: adds `log.access`, `log.max_size` and `log.max_age`; durations
  accept `h` and `d`; a new size value kind.

## Impact

- **Code:**
  - `proxlet-server`: an access-record hook in the engine, recording when the
    response body ends.
  - `proxlet`: the log writer task with rotation and retention, a `tracing`
    layer for event records, the `logs` command, its completion, and the
    `Logs` stub replaced.
  - `proxlet-core`: settings and value kinds.
- **External state:** up to 100 MB (by default) of log files in the state
  directory.
- **Dependencies:** none new. `serde_json`, `tracing` and
  `tracing-subscriber` are already used.
- **Docs:** `docs/usage.md` (logs section, settings table), README.
