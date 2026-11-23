# Tasks

## 1. Rule model and config file (proxlet-core)

- [ ] 1.1 Add route and target types with parsing per the `proxy-rules` spec; verify with unit tests covering every Route syntax and Target syntax scenario
- [ ] 1.2 Add the settings model with defaults, key lookup, value parsing (booleans, durations) and closest-key suggestion; verify with unit tests for each key and for invalid values
- [ ] 1.3 Add `proxlet.toml` load and comment-preserving save (temp file + rename, advisory lock); verify with tests that a round trip keeps comments and that a malformed file is reported with its line and never overwritten
- [ ] 1.4 Add location resolution (`PROXLET_HOME`, per-user, root) for config, socket and state; verify with unit tests for each case
- [ ] 1.5 Add the immutable rule table with host-then-longest-prefix matching on segment boundaries; verify with unit tests for every Rule matching scenario plus a property test against a naive matcher

## 2. HTTP engine (proxlet-server)

- [ ] 2.1 Add the listener and request dispatch over an atomically swappable snapshot, with the 404 response for unmatched requests; verify with an integration test that swaps rules mid-traffic without failed requests
- [ ] 2.2 Add reverse proxying with streamed bodies, preserved path and query, forwarding headers and hop-by-hop stripping; verify with integration tests against a local test upstream, including a chunked slow response
- [ ] 2.3 Add upgrade passthrough; verify with an integration test that echoes WebSocket messages through proxlet
- [ ] 2.4 Add 502 and 504 handling using the timeout settings; verify with integration tests for a closed port and a stalled upstream
- [ ] 2.5 Add static file serving with content types, conditional and range requests, index files and directory listings; verify with integration tests for each Serving files and Directories scenario
- [ ] 2.6 Add containment and dotfile checks; verify with integration tests for `..`, encoded traversal, an escaping symlink and `.env`
- [ ] 2.7 Add graceful shutdown with a bounded grace period; verify with an integration test that an in-flight request completes after the shutdown signal

## 3. Daemon and control channel (proxlet)

- [ ] 3.1 Implement `proxlet run`: load config, bind, serve, shut down on signal; verify by running it under `PROXLET_HOME` on a high port and requesting a rule
- [ ] 3.2 Add the bind-failure path that names the port and its holder; verify with a test that occupies the port first and asserts the error and exit status
- [ ] 3.3 Add the control socket server and client (ping, status, apply change) and the already-running check; verify with an integration test that a second `proxlet run` exits non-zero
- [ ] 3.4 Implement the validate → apply → persist pipeline for both the running and the stopped case; verify with tests that a rejected change leaves the file byte-identical

## 4. Rule and settings commands (proxlet)

- [ ] 4.1 Implement `proxlet add` including the not-listening warning and the "updated from" output; verify with CLI tests for each Validation, Atomic persist and Replacing scenario
- [ ] 4.2 Implement `proxlet rm`; verify with CLI tests for existing and unknown routes
- [ ] 4.3 Implement `proxlet ls` with table output on a terminal and tab-separated output otherwise; verify with CLI snapshot tests for both, and for the empty case
- [ ] 4.4 Implement `proxlet config` list, read and change, including live apply and the restart-needed notice; verify with CLI tests for each settings scenario
- [ ] 4.5 Add dynamic completion for `rm` routes and for `config` keys and values; verify with tests that drive the completion entry point and assert the candidates
- [ ] 4.6 Document rules, targets, matching and settings in the README and `docs/`; verify every documented command runs as written against a local build

## 5. Service lifecycle (proxlet-service, proxlet)

- [ ] 5.1 Define the `ServiceManager` trait and `detect()`; verify with unit tests using a fake manager, including the "none found" case
- [ ] 5.2 Implement the systemd manager (unit generation, install, enable, start, stop, disable, status, last-error retrieval) behind an injectable command runner; verify with unit tests asserting the generated unit and the `systemctl` invocations
- [ ] 5.3 Implement `proxlet start` and `proxlet stop`, including the invoking-user resolution and the unsupported-manager message; verify with CLI tests against the fake manager for every Start and Stop scenario
- [ ] 5.4 Implement `proxlet status` with its exit status; verify with CLI tests for running and not running
- [ ] 5.5 Document installation, `start`/`stop`, where files live and how to add a service manager; verify the contributor steps by following them for a stub manager

## 6. End-to-end verification

- [ ] 6.1 Add a CI job that runs the systemd flow in a systemd-enabled container: `start`, `add`, request, `stop`; verify the job passes
- [ ] 6.2 Run a load test (for example `oha`) through a proxy rule and a files rule and record throughput and p99 against a direct-to-upstream baseline in `docs/`; verify no errors under load and note the overhead
- [ ] 6.3 Run `openspec validate add-core-proxy --strict` and confirm every scenario in `specs/` maps to a passing test

## Workflow follow-up

- Archive the change once it is reviewed and merged.
