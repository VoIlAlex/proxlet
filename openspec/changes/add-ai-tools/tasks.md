# Tasks

## 1. Shared operations (proxlet)

- [x] 1.1 Add `proxlet::ops` (design decision 2) with these functions, which
  return data or a `Failure` and print nothing:
  - `add_rule` with `FrontPolicy`;
  - `remove_rule` and `set_setting`;
  - `rules`, `settings` and `logs` with a selector and a limit.

  Make `commands::rules`, `commands::config` and `commands::logs` use it.
  Verify that every existing `cli::`, `logs::` and `attach::` test still
  passes unchanged, and add unit tests for `FrontPolicy::Allow` on both
  answers.
- [x] 1.2 Record an info event when the daemon applies a change:
  - `rule added: …`, `rule updated: … (was …)`, `rule removed: …`;
  - `setting changed: KEY VALUE (was …)`.

  Verify with a daemon test that `proxlet add`, `proxlet rm` and `proxlet
  config` appear in `proxlet logs --all`.

## 2. MCP server (proxlet)

- [x] 2.1 Add the JSON-RPC loop for `proxlet mcp` (decision 1): one line per
  message; `initialize` with version negotiation; `ping`; notifications with
  no reply; errors -32700, -32601 and -32602; exit 0 when stdin closes;
  nothing but protocol messages on stdout. Add `Mcp` as a hidden subcommand.
  Verify with unit tests over an in-memory reader and writer, and with
  `tests/mcp.rs` tests for "Handshake", "Unknown protocol version", "Unknown
  method", "Malformed input" and "Client goes away".
- [x] 2.2 Add `tools/list` with the eight tools, their input schemas and
  annotations (decision 3), and the read tools `status`, `list_rules`,
  `get_settings`, `read_logs` and `get_stats` on top of `ops`, control and
  logs. Verify with `tests/mcp.rs` tests for:
  - "Listing tools", "Rules", "Logs for a route" and "Bad selector";
  - "Not running" and "Stats need the server";
  - a running-server `status` and `get_stats`.
- [x] 2.3 Add `add_rule`, `remove_rule` and `set_setting` on top of `ops`:
  absolute directory targets only, `allow_front_conflict`, and results with
  `outcome`, `previous` and `live`. Verify with `tests/mcp.rs` tests for:
  - "Add a rule" (served at once), "Invalid change" and "Remove a missing
    rule";
  - "Server stopped" and "Relative directory";
  - "Host served by nginx" and "Added anyway", using the fake nginx harness.

## 3. AI tool registration (proxlet-ai)

- [x] 3.1 Add the `AiTool` trait, `Server`, `Connection`, `Env`, `AiError`,
  `tools()` and `TOOL_IDS` (decision 4). Add `serde_json` with
  `preserve_order`, and check with `cargo tree` that no new crates are
  pulled in. Verify with a unit test that `tools()` lists `claude-code` and
  `cursor` in order.
- [x] 3.2 Add `cursor`: detection, status from `~/.cursor/mcp.json`, and
  connect and disconnect that keep other keys in order, write the backup,
  replace the file atomically keeping its mode, and refuse unparseable
  files. Verify with unit tests over a temporary home for:
  - "Cursor file created" and "Existing servers kept" (exact file contents);
  - "Unreadable file" (file unchanged), disconnect keeping other entries,
    and status for each of the three states.
- [x] 3.3 Add `claude_code`: detection on `PATH`, status read from
  `$CLAUDE_CONFIG_DIR/.claude.json` or `~/.claude.json`, connect (`remove` if
  present, then `add --scope user`), and disconnect. Verify with unit tests
  that use a recording `CommandRunner` for:
  - the exact `claude` arguments;
  - a failing `claude mcp add` reported with its output;
  - status for each state, including connected to another path.

## 4. `proxlet ai` (proxlet)

- [x] 4.1 Replace the `Ai` stub with `proxlet ai [<tool>…] [--yes]` and
  `proxlet ai rm [<tool>…]` (decision 5), with tool-name completion. Verify
  with CLI parse tests and a completion test for "Complete a tool".
- [x] 4.2 Implement the overview, the targets, the confirmation and
  connecting, and `rm`. The flow takes a confirm callback, as
  `rules::Conflict::decide` does. Extend the test harness to set `HOME` and
  `PATH`, and add a fake `claude` script (decision 6). Verify with:
  - unit tests for "Confirmed" and "Declined" through the callback;
  - `tests/ai.rs` tests for "Overview", "Nothing installed" and "No
    terminal";
  - `tests/ai.rs` tests for "One tool", "Already connected" and "Repairing a
    moved binary";
  - `tests/ai.rs` tests for "Claude Code", "Remove everywhere" and "Not
    connected".
- [x] 4.3 Write an "AI tools" section in `docs/usage.md`: what `proxlet ai`
  changes and where, the backup, `proxlet ai rm`, the MCP tools and their
  limits, and registering `proxlet mcp` by hand with other tools.

## 5. End to end and release notes

- [x] 5.1 Run a manual check with the real tools on this machine, after
  backing up their config first: `proxlet ai --yes` connects Claude Code and
  Cursor; `claude mcp list` shows `proxlet` as connected; a Claude Code
  session can call `list_rules` and `add_rule`; and `proxlet ai rm` restores
  both configs. Note the results in `verification.md`.
- [x] 5.2 Update `README.md` (status, roadmap) and `CHANGELOG.md`, and write
  `verification.md` mapping every scenario to its test. Verify with
  `openspec validate add-ai-tools --strict`.

## Workflow follow-up

- Push the branch, open a PR, and merge once CI passes.
- Archive the change and sync its specs.
