# Proposal

## Why

Developers increasingly ask an AI coding tool to wire up their local setup:
"proxy `/api` to the backend I just started", or "why is `/api` returning
502?". Today the tool can only do that by shelling out to `proxlet` and
parsing human-oriented output. An MCP server gives it structured access to the
same operations, with the same validation as the CLI. `proxlet ai` is the last
command the README promises, and it is the last item on the roadmap.

## What Changes

- **`proxlet mcp`** is an MCP server over stdio. AI tools launch it themselves,
  and it runs as the user who registered it.
  - **Tools to read:** server status, rules, settings, recent log records
    (with the same selectors as `proxlet logs`), and live statistics per rule.
  - **Tools to change things:** add or update a rule, remove a rule, and
    change a setting. These go through exactly the path the CLI uses:
    validated first, written atomically, rolled back if applying fails, and
    applied live when the server runs.
  - **Safety:** a rule for a host that nginx already serves is refused unless
    the call says to add it anyway, mirroring the CLI's question. A directory
    target must be an absolute path, because the server's working directory
    is whatever the AI tool chose.
- **`proxlet ai`** connects proxlet to installed AI tools.
  - It lists the supported tools it finds, with whether each is already
    connected, and asks before registering. `--yes` skips the question, and
    tool names (`claude-code`, `cursor`) limit it to those tools.
  - It registers at user scope, so the server is available in every project.
    The registration points at the absolute path of the running `proxlet`
    binary.
  - Running it again repairs a registration that points at another binary.
  - `proxlet ai rm [<tool>…]` unregisters.
  - Without a terminal and without `--yes`, it changes nothing and says how
    to proceed.
- **Supported tools in this change:** Claude Code and Cursor.
  - Claude Code is registered with its own `claude mcp` commands.
  - Cursor's `~/.cursor/mcp.json` is edited in place. Other entries are kept,
    a backup is written first, and the file is replaced atomically.
  - Each tool is one module behind one trait in `proxlet-ai`, so adding
    another tool touches nothing else.
- Every new command, flag and tool name tab-completes.

### Out of scope

- Certificates (`cert`), and starting or stopping the service, through MCP.
  These stay CLI-only, because they talk to outside services or need root.
  Not planned for now.
- Other AI tools (Claude Desktop, VS Code, Windsurf, Codex and others). Each is
  a later, small change that adds one module.
- Project-scope registrations (`.mcp.json`, `.cursor/mcp.json` in a
  repository): not planned for now.
- MCP over HTTP or SSE, MCP resources and prompts, and following logs as a
  stream: not planned for now. The tools return snapshots.

## Capabilities

### New Capabilities

- `mcp-server`: the `proxlet mcp` server, covering its protocol behaviour,
  which tools it offers, what each returns, and how changes are validated and
  refused.
- `ai-tools`: `proxlet ai` and `proxlet ai rm`, covering detection,
  connection status, registration with Claude Code and Cursor, backups, and
  confirmation.

### Modified Capabilities

None. Rules, settings, logs and statistics behave as they do today. The MCP
tools reuse them.

## Impact

- **Code:**
  - `proxlet-ai`: the `AiTool` trait, plus the `claude_code` and `cursor`
    modules, using the existing `CommandRunner` for the `claude` CLI.
  - `proxlet`: the `mcp` command (JSON-RPC over stdio), the `ai` command and
    its completion, and the `Ai` stub replaced.
- **External state:**
  - Claude Code's user configuration, changed through its CLI.
  - `~/.cursor/mcp.json`, plus `~/.cursor/mcp.json.proxlet-backup`.
- **Dependencies:** none new. `serde_json` is already used; the MCP protocol
  is small enough to implement directly. `proxlet-ai` gains `serde_json` and
  `proxlet-core`.
- **Docs:** `docs/usage.md` (an "AI tools" section), README, CHANGELOG.
