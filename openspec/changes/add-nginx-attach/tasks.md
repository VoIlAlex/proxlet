# Tasks

## 1. Settings and port ownership (proxlet-core, proxlet)

- [x] 1.1 Add `attach.enabled` (`on`, live) and `attach.port` (`47080`, on restart) to the settings table; verify with unit tests for defaults and parsing, and a CLI test for the "Attach port needs a restart" scenario
- [x] 1.2 Add the always-on internal listener on `127.0.0.1:<attach.port>`, sharing the engine with the public listener; verify with a daemon test that a rule is served on both
- [x] 1.3 Add the port-ownership task with the `Standalone` / `Attached` / `Waiting` / `Released` states, a 2 s bind retry, one log line per holder change, and release that keeps in-flight requests; verify with unit tests over a fake binder and clock, and a server test that an in-flight request completes across a release
- [x] 1.4 Replace exit-on-`EADDRINUSE` with waiting; report the mode and holder in `StatusInfo` and `proxlet status`; verify with daemon tests for "Port taken", "Port freed while waiting" and "Running standalone"
- [x] 1.5 Add the control requests `release` and `reclaim`, and the hidden `proxlet handoff release|reclaim` commands (wait up to 3 s, always exit 0); verify with daemon tests that `release` frees the public port while the internal one keeps serving, `reclaim` takes it back, and both exit 0 when proxlet is not running
- [x] 1.6 Trust `X-Forwarded-For` and `X-Forwarded-Proto` on the internal listener only; verify with a server test comparing the headers an upstream receives through each listener

## 2. nginx config reading (proxlet-front)

- [x] 2.1 Add the `FrontServer` trait, `Installation`, `FrontConfig`, `Plan` and `FrontError`, taking an injectable `CommandRunner`; verify that the crate builds and has docs
- [x] 2.2 Add an nginx config tokenizer and reader that follows `include` globs relative to the config prefix and collects `listen` (port, `default_server`, file:line), `server_name` and `error_log`; verify with fixture trees for the Debian, RHEL and Homebrew layouts, plus quoting, comments and a missing include
- [x] 2.3 Add `claims(host, port)` with nginx's name precedence (exact, leading wildcard, trailing wildcard, regex) and `default_server(port)`; verify with unit tests for each kind and an uncompilable regex
- [x] 2.4 Add nginx detection (`nginx -V` for the config path and the compiled-in error log), `is_running`, holder recognition (decision 6) and `is_bind_failure` for Linux and macOS error lines; verify with a recording runner and sample outputs

## 3. Applying and undoing nginx changes (proxlet-front)

- [x] 3.1 Add `plan_attach`: choose the included directory, render the catch-all block, and add the optional `default_server` move; verify with snapshot tests for each fixture layout, and that a declined takeover yields an empty plan
- [x] 3.2 Add the ledger and the generic plan applier (write ledger, atomic writes, `nginx -t`, reload if running, restore on failure); verify with tests using a fake runner that fails at each step and leaves the tree byte-identical
- [x] 3.3 Add detach from the ledger, including hash checks and re-finding a moved `listen` directive; verify with tests for a clean detach, a user-edited file and a vanished directive (warn, leave the file alone)

## 4. Service hooks (proxlet-service)

- [x] 4.1 Add `hook(unit, before_start, after_stop)` and `unhook(unit)` to `ServiceManager`; implement them for systemd as a drop-in with `-`-prefixed `ExecStartPre` and `ExecStopPost`, plus `daemon-reload`; verify with recording-runner tests for the drop-in text and commands
- [x] 4.2 Add supplementary groups to `ServiceSpec` and to the rendered unit; verify with a unit-file test
- [x] 4.3 Update `docs/extending.md` for the new `ServiceManager` methods and add an "Adding a front server" section

## 5. CLI: start, stop, add, ls, status (proxlet)

- [ ] 5.1 Make `sudo proxlet start [--yes]` set up the attach when nginx is detected and `attach.enabled` is on: plan, prompt for the default-server takeover, apply, hook the unit, add the log group, and be idempotent when already running; verify with fake-manager and fake-front tests for "Take over with confirmation", "Take over without a prompt", "Declined", "nginx rejects the change" and "Setting up the attach"
- [ ] 5.2 Make `sudo proxlet stop` detach (unhook, undo the ledger, reload) before stopping the service; verify with fake tests for "Stop while attached"
- [ ] 5.3 Make `proxlet run` set up the attach only when the nginx config directory is writable; verify with a test using a writable fixture config
- [ ] 5.4 Add the conflict check and prompt to `proxlet add [--yes]`; verify with CLI tests against a fixture nginx config for "Conflict confirmed", "Conflict without a terminal" and "Conflict declined"
- [ ] 5.5 Add the fourth `ls` field, "served by nginx (file:line)", in both table and plain output; verify with a CLI test for "Rule shadowed by nginx"
- [ ] 5.6 Show `attached (nginx)`, the `behind nginx` listen line and the "not forwarding yet" hint in `proxlet status`; verify with CLI tests for "Running behind nginx" and "nginx installed later"
- [ ] 5.7 Make `attach.enabled off` skip all nginx reading, writing and hooks; verify with a CLI test for "Attach disabled"

## 6. Watching nginx (proxlet)

- [ ] 6.1 Follow nginx's error log while `Standalone` and release on a bind failure for the HTTP port; verify with a daemon test that appends a bind-failure line to a fixture log and asserts the port is freed within 500 ms
- [ ] 6.2 Add the process-watch fallback for an unreadable log, and reclaim from `Released` once no nginx runs (10 s at most); verify with unit tests over a fake process source and clock for "nginx fails for another reason"

## 7. End to end and docs

- [ ] 7.1 Change the CI `systemd` job's taken-port step to expect waiting, and the failed-start step to use an invalid config file; verify that the job passes
- [ ] 7.2 Add a CI job that installs nginx and runs the whole flow: start with a takeover, an unclaimed host through nginx, a claimed host, a WebSocket, `systemctl stop` and `start nginx`, a manual `nginx`, an `add` conflict, and `stop` restoring the config byte-for-byte; verify that the job passes
- [ ] 7.3 Document attach mode in `docs/usage.md` (modes, what proxlet writes, takeover, conflicts, handoff, `attach.*` settings, uninstalling), and update `README.md` and `CHANGELOG.md`
- [ ] 7.4 Record each spec scenario's test in `verification.md`, and run `openspec validate add-nginx-attach --strict`

## Workflow follow-up

- Archive with `openspec-archive-change` after the PR is merged.
- The next change is `add-cert` (port 443 and TLS, including nginx forwarding HTTPS).
