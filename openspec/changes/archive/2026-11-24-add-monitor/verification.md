# Verification

This file maps each scenario in `specs/` to the test that exercises it. Paths
are relative to `crates/`.

## How the tests work

- **`proxlet-server/tests/stats.rs`** drives the engine with real HTTP and
  WebSocket clients and reads `Engine::stats()`.
- **`monitor::`** is `proxlet/tests/monitor.rs`, an integration suite.
  - It runs the real `proxlet run` in an isolated `PROXLET_HOME` and reads the
    `stats` control reply.
  - It also runs `proxlet monitor` piped, with `--once`, without a server,
    and on a pseudo-terminal.
- **Unit tests** cover the engine's statistics and the CLI's rendering:
  - `proxlet-server/src/stats.rs` and `proxlet-server/src/snapshot.rs` cover
    the windows, histogram and carry-over.
  - `proxlet/src/monitor/{format,view,tui}.rs` cover cell text, sorting,
    keys, reconnecting and drawing through ratatui's `TestBackend`.
- **The CI `systemd` job** runs `proxlet monitor --once` against the real
  service on GitHub's Ubuntu runner, checks that piped output has no escape
  sequences, and checks the exit status once the service has stopped.

Some timings are shorter than in the spec, to keep the suite fast. The
behaviour checked is the same:
- *Latency is time to the response head* streams for 2 s instead of 5 s.
- *Request in flight* waits 0.8 s instead of 3 s.

On macOS, `cargo test --workspace` passes (386 tests), and `cargo clippy
--workspace --all-targets -- -D warnings` and `cargo fmt --check` are clean.
The statistics and monitor suites passed 5 times in a row.

## monitor

| Scenario | Test |
| --- | --- |
| Requests counted per rule | `stats.rs` `requests_counted_per_rule` |
| Unmatched requests | `stats.rs` `unmatched_requests_go_to_the_no_rule_row` |
| Access logging off | `stats.rs` `counted_with_access_logging_off` |
| Restart | `monitor::totals_start_from_zero_after_a_restart` |
| New rule | `monitor::rows_follow_rule_changes` |
| Removed rule | `monitor::rows_follow_rule_changes`; `proxlet-server/src/snapshot.rs` `statistics_follow_routes_across_snapshots` |
| Target changed | `monitor::rows_follow_rule_changes`; `snapshot.rs` `statistics_follow_routes_across_snapshots`; `stats.rs` `statistics_survive_a_rule_change` |
| Errors | `stats.rs` `upstream_errors_are_counted`; `monitor::snapshot_over_the_control_socket` |
| Upstream's own 5xx | `stats.rs` `an_upstreams_own_500_is_not_an_upstream_error` |
| Latency | `stats.rs` `latency_percentiles`; `proxlet-server/src/stats.rs` `percentiles_and_status_classes`, `buckets_are_ordered_and_midpoints_close` (error at most 6.25%) |
| Latency is time to the response head | `stats.rs` `latency_is_time_to_the_response_head` |
| No recent traffic | `proxlet-server/src/stats.rs` `old_slots_are_ignored_after_a_gap`, `no_traffic_has_no_latency`; `proxlet/src/monitor/format.rs` `rates_latencies_and_sizes` (`-`) |
| WebSocket open | `stats.rs` `open_websockets_are_counted` |
| Request in flight | `stats.rs` `request_in_flight`; `proxlet-server/src/stats.rs` `gauges_follow_guards` |
| Heavy traffic | `proxlet-server/src/stats.rs` `concurrent_recording_counts_every_request` (8 threads, 400,000 requests, none lost). By construction, recording uses only relaxed atomic operations on fixed-size arrays allocated with the rule, with no lock and no allocation; memory is 60 slots × 209 buckets per rule, whatever the traffic |
| Snapshot | `monitor::snapshot_over_the_control_socket`; `proxlet/src/control.rs` `stats_wire_format_is_stable`; `stats.rs` `open_connections_are_counted` |
| Watching traffic | `monitor::live_table_in_a_terminal` (the table appears on a real pty); manual pty check below |
| Sorting | `proxlet/src/monitor/view.rs` `sorts_cycle_with_no_rule_last`; `monitor/tui.rs` `sort_marker_moves`; manual pty check below |
| Quit | `monitor::live_table_in_a_terminal` (`q` exits 0 and leaves the alternate screen); `view.rs` `quit_keys` |
| Restart while watching | `view.rs` `restart_while_watching`; `tui.rs` `not_running_banner`; manual pty check below |
| Piped | `monitor::piped_prints_one_plain_snapshot`; `format.rs` `plain_table_is_aligned`; CI `systemd` job |
| Once in a terminal | `monitor::once_prints_the_same_table`; CI `systemd` job |
| Not running | `monitor::not_running_says_how_to_start`; CI `systemd` job |

Scrolling is covered by `view.rs` `scrolling_stays_in_bounds`, and completion
of `--once` by `monitor::monitor_flags_complete`.

## Manual check in a real terminal

A script ran the built `proxlet monitor` on a pseudo-terminal (140×20) and
read the screen through a terminal emulator (`pyte`). It checked the
following, and all of them passed:

1. The table and the status line appear.
2. After 5 requests to `/api` and 3 to a rule whose upstream is down, the
   rows show `TOTAL 5` and `UP-ERR 3` within 2.2 seconds.
3. `s` marks `REQ/S▼`. A second `s` marks `4XX▼`/`5XX▼` and puts the failing
   rule first.
4. When the server stops, the screen says "proxlet is not running —
   reconnecting…".
5. When the server starts again, rows come back with `TOTAL 0`.
6. `q` exits 0, leaves the alternate screen, and shows the cursor again.
