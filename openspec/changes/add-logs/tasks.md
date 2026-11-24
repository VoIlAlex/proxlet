# Tasks

## 1. Settings (proxlet-core)

- [x] 1.1 Accept `h` and `d` in durations and format them in the largest exact unit; add `Kind::Size` (`KB`/`MB`/`GB`, powers of 1024) with formatting; verify with unit tests for parsing, formatting and rejected values
- [x] 1.2 Add `log.access` (`on`), `log.max_size` (`100MB`, at least `10MB`) and `log.max_age` (`7d`), all live; verify with unit tests for defaults, and CLI tests for "Days and sizes" and "Size too small"
- [x] 1.3 Document the settings and the new value forms in the settings table of `docs/usage.md`

## 2. Access records in the engine (proxlet-server)

- [x] 2.1 Add `AccessRecord`, the `AccessLog` trait and `Engine::set_access_log`; record the client (peer, or the first `X-Forwarded-For` entry from the front origin), method, lowercase host without port, path without query, rule and target in `handle_from`, including challenge, redirect and no-rule answers; verify with server tests that use a recording sink for "Proxied request" (no query), "Behind nginx", a no-rule request and a redirect
- [x] 2.2 Wrap response bodies in a counting body that emits the record once from `Drop`, with status, bytes and duration; pass the proxy error reason out of `proxy::forward`; skip recording when `log.access` is off; verify with server tests for "Upstream down", "Streaming response" (total bytes and full duration), a client hanging up mid-stream, a WebSocket upgrade (one record, status 101), and "Access logging off" applied live

## 3. Writer, rotation and retention (proxlet)

- [x] 3.1 Add the record types and their JSON form (decision 1) with the RFC 3339 millisecond timestamp helper; verify with unit tests for the exact JSON of a request, an error request and an event, and for the timestamp at a known instant
- [x] 3.2 Add the writer task: bounded queue (8192), `try_send` with a drop counter, buffered writes flushed every 200 ms, a recorded drop event, and an `eprintln!` warning (at most once a minute) with reopening every 5 s on write errors; verify with tests over a temporary directory for ordering, flushing, the drop event, and an unwritable directory never blocking senders
- [x] 3.3 Add rotation at the file size limit (10 MB; `PROXLET_TEST_LOG_FILE_BYTES` in tests) and retention by total size and age at startup, after rotation and hourly, reading the limits live from the snapshot; `0700` directory, `0600` files; verify with tests for "Rotation and size limit" (no split records), "Age limit", the current file never being deleted, and "Log limits apply live"
- [x] 3.4 Install a `tracing` layer for `proxlet run` that sends info-and-above events from `proxlet*` targets to the writer alongside the stderr output; connect the engine's sink to the writer in the daemon and drain the queue on shutdown; verify with daemon tests that requests and "Port taken" appear in the log files, and that stderr output is unchanged

## 4. `proxlet logs` (proxlet)

- [ ] 4.1 Replace the `logs` stub: `proxlet logs [<selector>] [--all] [--json]` with selector parsing (port, route, domain) and its errors; verify with CLI parse tests and a test that an invalid selector exits non-zero
- [ ] 4.2 Implement `--all` (oldest first across files) and the default last-20-then-follow, with a `Follower` that survives rotation through the open handle (decision 6); verify with CLI tests for "All history", "Follow" (a new request appears within a second), "Rotation while following" (no loss or repeat, using the small file size hook), no records yet, and reading while the server is stopped
- [ ] 4.3 Implement the output (decision 7): aligned terminal lines with status and level colours, the same text without colour when piped, `--json` byte for byte; filtering by selector, with events shown only without one; verify with unit tests for line formatting and CLI tests for "By route", "By domain", "By upstream port", "Piped" and "JSON"
- [ ] 4.4 Add selector completion (rule routes, host-rule domains, upstream ports); verify with a completion test for "Complete a selector"
- [ ] 4.5 Document logs in `docs/usage.md`: what is recorded and what never is, the JSON fields, selectors, following, retention, and where the files are

## 5. End to end and release notes

- [ ] 5.1 Extend the CI `systemd` job: after requests to the running service, `proxlet logs --all` shows them and an event, and `proxlet logs /site --all --json | python3 -m json.tool` parses; verify that the job passes
- [ ] 5.2 Update `README.md` (status, roadmap) and `CHANGELOG.md`, and write `verification.md` mapping every scenario to its test; verify with `openspec validate add-logs --strict`

## Workflow follow-up

- Push the branch, open a PR, and merge once CI passes.
- Archive the change and sync its specs.
