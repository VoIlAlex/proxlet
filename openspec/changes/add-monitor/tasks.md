# Tasks

## 1. Statistics in the engine (proxlet-server)

- [x] 1.1 Add `RuleStats` (design decisions 2 and 3):
  - lifetime totals and gauges;
  - the ring of 60 one-second slots, claimed and reset with `compare_exchange`;
  - the log-linear latency histogram, with percentiles and windowed sums read
    from a given second.

  Verify with unit tests:
  - bucket boundaries and percentile midpoints within 9%;
  - rates over the last 10 complete seconds;
  - slots older than the window ignored after a gap;
  - `-` (`None`) latency with no traffic;
  - a multi-threaded hammer test whose total equals the requests sent.
- [x] 1.2 Give each `ProxyTarget` and `FilesTarget` an `Arc<RuleStats>`, and
  the snapshot an `unmatched` one (decision 1). Make `Engine::replace` carry
  the statistics over by route text, including `unmatched`. Verify with unit
  tests:
  - an unchanged route keeps its `Arc` (`Arc::ptr_eq`);
  - a changed target keeps it;
  - a removed and re-added route gets a fresh one.
- [x] 1.3 Record in `handle_from` (decision 4):
  - at the response head: requests, status class, upstream errors read before
    access logging takes the extension, and latency;
  - bytes per frame and the in-flight gauge, through a body wrapper that is
    now always applied and merged with `Counted`;
  - challenges, redirects and no-rule answers into `unmatched`.

  Verify that the `access.rs` tests still pass, and add server tests
  (`tests/stats.rs`) for:
  - "Requests counted per rule";
  - "Unmatched requests";
  - "Access logging off";
  - "Errors";
  - "Upstream's own 5xx";
  - "Latency";
  - "Latency is time to the response head";
  - "Request in flight".
- [x] 1.4 Count open WebSocket tunnels in `proxy::forward` with a guard
  (decision 4), and open client connections in `serve_io`, counted after the
  TLS handshake (decision 5). Add `Engine::stats(): StatsSnapshot`, with rows
  in table order and the "no rule" row last. Verify with server tests for
  "WebSocket open" (1 while open, 0 after close) and the connection count
  rising and falling.

## 2. Control request (proxlet)

- [x] 2.1 Add `Request::Stats`, and add `StatsReply`/`StatsRow` on `Reply`,
  with the wire form in decision 6. Answer the request in the daemon from
  `Engine::stats` and its uptime. Verify with:
  - a wire-format unit test for the exact JSON;
  - a daemon test for "Snapshot" (one row per rule plus "no rule");
  - daemon tests for "New rule", "Removed rule", "Target changed" and
    "Restart" through `proxlet add`/`rm` and restarting.

## 3. `proxlet monitor` (proxlet)

- [x] 3.1 Add `ratatui` (0.30, crossterm backend) to `proxlet`, and check that
  the MSRV of 1.89 still builds. Replace the `Monitor` stub with
  `proxlet monitor [--once]`, and make the command tab-complete. Verify with
  CLI parse tests and a completion test listing `--once`.
- [x] 3.2 Add `monitor::format` for cell text: rates, counts, durations
  (`12ms`, `1.2s`, `-`) and bytes per second (`0B/s`, `20KB/s`, `1.5MB/s`).
  Add the plain snapshot renderer (decision 7). Verify with unit tests for
  every format, and for an exact plain table with aligned columns.
- [x] 3.3 Add the snapshot path: when stdout is not a terminal or `--once` is
  given, send one `stats` request and print. When the server is not running,
  fail with `proxlet is not running` and a `proxlet start` hint, exit 1. When
  the server is too old, say so and exit 1. Verify with CLI tests for:
  - "Piped" (header `RULE`, rows `/`, `/api`, `(no rule)`, no escape bytes,
    exit 0);
  - "Once in a terminal" through `--once`;
  - "Not running" (stderr text, exit 1).
- [x] 3.4 Add `monitor::view`: state, sort cycling with the "no rule" row
  pinned last, scrolling, keys (`s`, arrows, `PgUp`/`PgDn`, `q`/`Esc`/Ctrl-C),
  and connected and reconnecting states (decision 8). Verify with unit tests
  for:
  - each sort order, including `-` last for p95;
  - scroll limits;
  - quit keys;
  - "Restart while watching", driven by a fake stats source that fails and
    then recovers with fresh totals.
- [x] 3.5 Add `monitor::tui`: the ratatui table with uptime and connections
  above it, the `▼` marker on the sort column, and the not-running banner.
  Add the event loop that polls stats each second and keys in between, with
  `ratatui::init`/`restore` on every exit path. Verify with:
  - `TestBackend` render tests for a table, the sort marker and the banner;
  - a manual check in a real terminal of "Watching traffic", "Sorting",
    "Quit" (terminal restored) and "Restart while watching", noted in
    `verification.md`.
- [x] 3.6 Write a "Monitor" section in `docs/usage.md`: the columns and their
  windows, keys, the snapshot mode, and that numbers reset on restart.

## 4. End to end and release notes

- [ ] 4.1 Extend the CI `systemd` job: after requests to the running service,
  `proxlet monitor --once` shows the rule's row with a non-zero total. Verify
  that the job passes.
- [x] 4.2 Update `README.md` (status, roadmap) and `CHANGELOG.md`, and write
  `verification.md` mapping every scenario to its test or manual check, with
  "Heavy traffic" covered by the hammer test and the no-lock,
  no-allocation design. Verify with `openspec validate add-monitor --strict`.

## Workflow follow-up

- Push the branch, open a PR, and merge once CI passes.
- Archive the change and sync its specs.
