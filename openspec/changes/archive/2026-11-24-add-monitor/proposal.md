# Proposal

## Why

`proxlet logs` shows what happened to individual requests, but not how a route
is doing right now: whether its traffic rate is normal, whether errors are
rising, or whether it got slow. A live per-rule table answers that at a glance.
`proxlet monitor` is the stub the README promises after logs, and the planned AI
tools will read the same numbers.

## What Changes

- **Live statistics in the daemon.** The running server keeps statistics in
  memory for each rule, plus one "no rule" row for everything else it answers:
  requests no rule matched, redirects to HTTPS, and ACME challenges. The
  numbers are:
  - requests per second over the last 10 seconds;
  - total requests since the server started;
  - 4xx and 5xx responses, and how many of the 5xx were proxlet failing to
    reach the upstream (502/504);
  - p50, p95 and p99 latency over the last 60 seconds;
  - bytes sent per second;
  - requests in flight, and open WebSocket tunnels.

  Statistics are kept whether or not `log.access` is on. Nothing is written to
  disk, and the numbers start again from zero when the server restarts.
- **Never on the request path.** Recording uses atomic counters in fixed-size
  structures: no locks, no allocation per request, and bounded memory per rule.
- **`proxlet monitor`** shows a full-screen table, one row per rule and the
  "no rule" row, refreshed every second.
  - **Keys:** `s` changes the sort column, the arrow keys scroll, and `q`,
    `Esc` or Ctrl-C quit.
  - **Server down:** if the server stops while the table is open, the table
    says so and reconnects when the server comes back.
  - **One snapshot:** when stdout is not a terminal, or with `--once`, it
    prints one plain aligned table and exits, so the output can be piped or
    used in scripts.
  - **Not running:** if the server is not running at all, it exits non-zero
    and says how to start the server.
- **The control socket** gains a `stats` request, so the CLI (and later the AI
  tools) can read the numbers.

### Out of scope

- The MCP server and `proxlet ai`, including exposing these statistics to AI
  tools: the *AI tools* change.
- Keeping history across restarts, charts over time, or exporting metrics
  (Prometheus, OpenTelemetry): not planned for now.
- Statistics per upstream instance, per client or per path below a rule: not
  planned. `proxlet logs` covers per-request detail.
- Filtering the monitor by selector: not planned for now. The table has one
  row per rule, which is usually few.

## Capabilities

### New Capabilities

- `monitor`: the live per-rule statistics the server keeps, the control-socket
  request that reads them, and `proxlet monitor` for showing them.

### Modified Capabilities

None. Access records and `log.access` behave as they do today.

## Impact

- **Code:**
  - `proxlet-server`: a statistics registry in the engine, fed from the same
    per-request hook as access records; WebSocket tunnel and in-flight gauges.
  - `proxlet`: a `stats` control request and reply, the `monitor` command
    (TUI and plain output), and the `Monitor` stub replaced.
- **Dependencies:** `ratatui` with its `crossterm` backend, for the terminal
  UI, in the `proxlet` crate only.
- **Docs:** `docs/usage.md` (a monitor section), README, CHANGELOG.
