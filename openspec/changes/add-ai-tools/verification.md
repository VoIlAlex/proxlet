# Verification

Each scenario in `specs/` mapped to the test that exercises it. Paths are
relative to `crates/`.

## How the tests are set up

- **`mcp::`** is `proxlet/tests/mcp.rs`. It spawns the real `proxlet mcp`
  with pipes and speaks JSON-RPC to it, as an AI tool does. Where a scenario
  needs a server it runs a real `proxlet run`, and where it needs nginx it
  uses the fake nginx tree. Everything lives in an isolated `PROXLET_HOME`.
- **`ai::`** is `proxlet/tests/ai.rs`. It runs the real `proxlet ai` with
  these overrides:
  - `HOME` set to the test's directory;
  - `PATH` holding a fake `claude` script, which records its arguments and
    maintains `~/.claude.json` like the real CLI;
  - `CLAUDE_CONFIG_DIR` removed.

  The developer's real tools are never touched.
- **Unit tests** cover the protocol and the tool modules:
  - `proxlet/src/mcp/mod.rs` covers the protocol through an in-memory
    reader and writer;
  - `proxlet-ai/src/{cursor,claude_code,lib}.rs` cover the tool modules
    with a temporary home and a recording `CommandRunner`;
  - `proxlet/src/commands/ai.rs` covers the connect flow with a confirm
    callback;
  - `proxlet/src/ops.rs` covers the front-server policy.

On macOS, `cargo test --workspace` passes (435 tests), and `cargo clippy
--workspace --all-targets -- -D warnings` and `cargo fmt --check` are clean.
No third-party crate was added: `serde_json`'s `preserve_order` uses
`indexmap`, already in the build.

## mcp-server

| Scenario | Test |
| --- | --- |
| Handshake | `mcp::handshake_errors_and_exit` (via `Mcp::start`, version `2025-06-18`); `proxlet/src/mcp/mod.rs` `initialize_negotiates_the_version` |
| Unknown protocol version | `mcp::handshake_errors_and_exit`; `mod.rs` `initialize_negotiates_the_version` |
| Unknown method | `mcp::handshake_errors_and_exit`; `mod.rs` `errors_keep_the_server_running` |
| Malformed input | `mcp::handshake_errors_and_exit`; `mod.rs` `errors_keep_the_server_running` |
| Client goes away | `mcp::handshake_errors_and_exit`, `only_protocol_messages_on_stdout` (exit 0, and stdout holds only JSON-RPC lines) |
| Listing tools | `mcp::listing_tools`; `proxlet/src/mcp/tools.rs` `eight_tools_with_schemas_and_annotations` |
| Rules | `mcp::reading_without_a_server` |
| Logs for a route | `mcp::logs_for_a_route_and_bad_selectors` |
| Bad selector | `mcp::logs_for_a_route_and_bad_selectors` (same message as `proxlet logs`) |
| Not running | `mcp::reading_without_a_server` |
| Stats need the server | `mcp::reading_without_a_server`; with a server: `mcp::status_and_stats_with_a_server` |
| Add a rule | `mcp::changes_apply_live` (served at once) |
| Invalid change | `mcp::invalid_changes_write_nothing` (same message as `proxlet config`; config file unchanged) |
| Remove a missing rule | `mcp::invalid_changes_write_nothing` |
| Server stopped | `mcp::changes_while_stopped_apply_at_start` |
| Relative directory | `mcp::invalid_changes_write_nothing` |
| Host served by nginx | `mcp::hosts_nginx_serves_need_allowing`; `proxlet/src/ops.rs` `conflict_answers_for_mcp` |
| Added anyway | `mcp::hosts_nginx_serves_need_allowing` |

The CLI's own conflict question still behaves the same:
`ops.rs` `conflict_answers_in_the_cli`, and the existing `attach::` tests.
Changes are recorded as log events (task 1.2):
`logs::configuration_changes_are_events`.

## ai-tools

| Scenario | Test |
| --- | --- |
| Overview | `ai::overview_then_connect` |
| Nothing installed | `ai::nothing_installed` |
| Confirmed | `proxlet/src/commands/ai.rs` `confirmed` |
| Declined | `commands/ai.rs` `declined_changes_nothing` |
| No terminal | `ai::no_terminal_changes_nothing`; `commands/ai.rs` `no_terminal_changes_nothing` |
| One tool | `ai::one_tool` |
| Already connected | `ai::already_connected`; `commands/ai.rs` `already_connected_asks_nothing` |
| Repairing a moved binary | `ai::repairing_a_moved_binary`, `ai::claude_code_is_registered_through_its_cli` |
| Claude Code | `ai::claude_code_is_registered_through_its_cli`, `ai::a_failing_claude_is_reported`; `proxlet-ai/src/claude_code.rs` `connect_runs_claude_mcp_add`, `a_failing_claude_is_reported_with_its_output` |
| Cursor file created | `ai::overview_then_connect`; `proxlet-ai/src/cursor.rs` `file_created` (exact contents, mode 0600) |
| Existing servers kept | `ai::existing_cursor_servers_are_kept_and_backed_up`; `cursor.rs` `existing_servers_and_keys_are_kept_in_order` (key order, backup, mode kept) |
| Unreadable file | `ai::an_unreadable_cursor_file_is_left_alone`; `cursor.rs` `unreadable_file_is_left_alone` |
| Remove everywhere | `ai::remove_everywhere` |
| Not connected | `ai::removing_from_a_tool_that_is_not_connected` |
| Complete a tool | `ai::tools_complete` |

Detection and status are covered by:
- `cursor.rs` `three_states`;
- `claude_code.rs` `three_states_and_another_path` and
  `config_dir_is_honoured`;
- `lib.rs` `programs_are_found_on_path_only_when_executable`.

Unknown tool names are covered by `commands/ai.rs`
`unknown_tools_are_rejected`, and `proxlet mcp` being hidden by
`cli::mcp_is_hidden_from_help`.

## Manual check with the real tools (task 5.1)

This ran on the developer's Mac on 2026-11-24, with `target/debug/proxlet`.

1. **Backups:** `~/.claude.json` was copied first; `~/.cursor/mcp.json` did
   not exist.
2. **Status, read-only:** `proxlet ai` without a terminal listed both tools
   as not connected, exited 1 with the `--yes` hint, and changed nothing.
3. **Connecting:** `proxlet ai --yes` connected Claude Code and Cursor.
   - `claude mcp get proxlet` reported `Scope: User config`, `Status: ✔
     Connected`, and the right command and `mcp` argument. That is Claude
     Code's own health check, so it completed the MCP handshake with
     `proxlet mcp`.
   - `~/.cursor/mcp.json` held exactly the `proxlet` entry.
4. **Reading from Claude Code:** a headless session (`claude -p … --allowedTools
   mcp__proxlet__list_rules`) called `list_rules` against the real
   configuration and answered 4. That matches `proxlet ls`.
5. **Changing from Claude Code:** a headless session used a sandbox MCP
   config with the same binary and a temporary `PROXLET_HOME`, so the real
   rules were not changed. It called `add_rule /mcp-check 8123` and then
   `list_rules`. `proxlet ls` in that sandbox showed the new rule.
6. **Disconnecting:** `proxlet ai rm` disconnected both tools.
   - Claude Code's `mcpServers` were identical to the backup.
   - Cursor was left with `{"mcpServers": {}}` and a backup. Both files had
     been created by this check, so they were removed, which restored the
     original state.
