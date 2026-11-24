# Design

## Context

Everything the MCP tools need already exists behind the CLI:
- **Rules and settings:** `commands::submit` sends a `Change` to the daemon's
  control socket, or applies it to the config file through `pipeline::apply`
  when the server is not running. Validation, the atomic write and rollback
  live there.
- **Status and statistics:** `control::running` gives status, and
  `Request::Stats` gives the statistics.
- **Logs:** `logs::read` and `logs::show::Selector` read and filter the log
  files without the daemon.
- **Front-server conflicts:** `rules::Conflict::decide` holds the CLI's check
  for hosts the front server already serves. It prints a warning and asks
  through a callback.

Extension points follow one pattern, a trait in its own crate with one module
per integration: `FrontServer` in `proxlet-front` and `ServiceManager` in
`proxlet-service`. `proxlet-ai` exists as an empty crate. External commands go
through `proxlet_core::command::CommandRunner`, so tests can fake them.

## Goals / Non-Goals

**Goals:**
- The MCP tools and the CLI share one code path for every operation, so their
  behaviour cannot drift.
- Tests never touch the developer's real `~/.claude.json`, `~/.cursor` or
  `claude` binary.

**Non-Goals:**
- A general MCP library. Only what proxlet needs is implemented.
- Long-running or streaming tools.

## Decisions

### 1. A small MCP implementation instead of an SDK

`proxlet mcp` reads stdin line by line, synchronously; no async runtime is
needed.

- **Messages:** each line is parsed as a JSON-RPC 2.0 message.
  - A request (it has an `id`) gets exactly one reply on stdout, flushed at
    once.
  - A notification, such as `notifications/initialized`, gets no reply.
  - Batches are not supported; the 2025-06-18 protocol version removed them.
  - A line that is not JSON gets error -32700 with `id: null`.
- **Methods:**
  - `initialize` echoes the client's `protocolVersion` if it is one of
    `2024-11-05`, `2025-03-26`, `2025-06-18` or `2025-11-25`, and otherwise
    answers the newest of these. Its capabilities are `{"tools":
    {"listChanged": false}}`, and its `serverInfo` gives the name `proxlet`
    and the crate version.
  - `ping` answers `{}`.
  - `tools/list` and `tools/call` are described below.
  - Anything else gets error -32601. Bad params for a known method get
    error -32602.
- **Results:** a tool result is `{"content":[{"type":"text","text":<pretty
  JSON>}],"isError":<bool>}`. Successful results also carry
  `structuredContent` with the same JSON, which newer clients read. A
  failure's text is the CLI's message, followed by its hints, one per line.
- **Output streams:** stdout carries only protocol messages. Diagnostics go to
  stderr, and the AI tools keep that in their logs.

The alternative was the `rmcp` SDK. It pulls in an async runtime layer,
macros and schema derivation for eight tools. The protocol surface here is
four methods, and a hand-written implementation is easy to test line by line.

### 2. Operations move out of the CLI printers into `ops`

A new module, `proxlet::ops`, holds functions that return data or a
`Failure` and print nothing:

- `add_rule(locations, route, target, cwd, front: FrontPolicy) -> Result<Changed, Failure>`
- `remove_rule`, `set_setting` → `Changed { outcome: WireOutcome, live: bool }`
- `rules(locations)`, `settings(locations)`, `logs(locations, selector, limit)`

`FrontPolicy` is either `Ask(&dyn Fn(&str) -> Option<bool>)`, used by the
CLI, or `Allow(bool)`, used by MCP. When MCP is refused, the failure says "nginx
serves site.example …; set allow_front_conflict to add it anyway".

`commands::rules`, `commands::config` and `commands::logs` call `ops` and keep
their printing, so the CLI's output does not change. The MCP tools call the
same functions.

For directory targets, MCP checks that the raw target text is an absolute
path *before* `Target::parse`. That covers `./site`, `site` and `../x`, which
the CLI resolves against its working directory. Without this check MCP would
resolve them against a directory the user never chose.

### 3. Tool shapes

| Tool | Arguments | Result JSON |
| --- | --- | --- |
| `status` | — | `{"running":false}` or `{"running":true, …StatusInfo}` |
| `list_rules` | — | `[{"route","kind","target"}]`, in `proxlet ls` order |
| `get_settings` | — | `[{"key","value","default","live","help"}]` |
| `read_logs` | `selector?`, `limit?` (1–500, default 50) | array of stored records (parsed JSON objects), oldest first |
| `get_stats` | — | the `StatsReply` wire form |
| `add_rule` | `route`, `target`, `allow_front_conflict?` | `{"outcome":"added"\|"updated","previous"?,"route","target","live"}` |
| `remove_rule` | `route` | `{"outcome":"removed","previous","route","live"}` |

When the server is not running, change results also carry a `note` saying the change applies when it starts.
| `set_setting` | `key`, `value` | `{"outcome":"setting_changed","previous","restart_needed","key","value","live"}` |

- **Annotations:** the read tools carry `"annotations": {"readOnlyHint":
  true}`. The three that change things carry `destructiveHint: false`,
  because they are reversible, and `idempotentHint: true`.
- **Descriptions** tell the model what a target can be (port, `host:port`, URL
  or absolute directory) and what a route looks like.
- **`read_logs`** uses `logs::read::last_matching` with the parsed
  `Selector`. Unlike `proxlet logs`, events are included only when there is
  no selector.

### 4. `proxlet-ai`: one trait, one module per tool

```rust
pub struct Server { pub command: PathBuf, pub args: Vec<String> } // name is always "proxlet"
pub enum Connection { NotInstalled, NotConnected, Connected { command: PathBuf } }
pub trait AiTool {
    fn id(&self) -> &'static str;          // "claude-code", "cursor"
    fn name(&self) -> &'static str;        // "Claude Code", "Cursor"
    fn status(&self) -> Result<Connection, AiError>;
    fn connect(&self, server: &Server) -> Result<(), AiError>;
    fn disconnect(&self) -> Result<bool, AiError>; // false: was not connected
}
pub struct Env { pub home: PathBuf, pub path: OsString, pub runner: Box<dyn CommandRunner> }
pub fn tools(env: &Env) -> Vec<Box<dyn AiTool>>
```

`Env` is built from `HOME` and `PATH` in production and from temporary
directories in tests. `proxlet` only ever iterates `tools()`, so a new tool is
one module plus one line in `tools()`.

**Claude Code** (`claude_code.rs`):
- **Installed:** a `claude` executable is on `PATH`.
- **Status:** read-only. The tool reads the top-level `mcpServers.proxlet`
  entry from `$CLAUDE_CONFIG_DIR/.claude.json` or `~/.claude.json`, its
  user-scope registrations. A missing file or entry means not connected.
  - Reading avoids `claude mcp get`, which health-checks every server, and
    whose output is not a stable format.
- **Connect:** if a `proxlet` entry exists, first run `claude mcp remove
  --scope user proxlet`, because `add` refuses to overwrite. Then run `claude
  mcp add --scope user proxlet -- <path> mcp`.
- **Disconnect:** `claude mcp remove --scope user proxlet`.
- **Writes:** Claude Code rewrites its file often, so proxlet never writes it
  directly.

**Cursor** (`cursor.rs`):
- **Installed:** `~/.cursor/` exists, or a `cursor` executable is on `PATH`.
- **File:** `~/.cursor/mcp.json`. It is parsed with `serde_json` and the
  `preserve_order` feature, which uses `indexmap`, already in the build
  through `h2`, so other keys keep their order.
- **Connect:** set `mcpServers.proxlet = {"command": <path>, "args": ["mcp"]}`.
  - The previous file is copied to `mcp.json.proxlet-backup`, overwriting an
    older backup.
  - The new file is written to a temporary file in the same directory, with
    the old file's mode or `0600`, then renamed over the old one.
  - The output is pretty-printed with two-space indentation and a trailing
    newline.
- **Disconnect:** remove the entry the same way. An empty `mcpServers`
  object is kept.
- **Parse errors:** `AiError::Parse { path, message }`; nothing is written.

### 5. `proxlet ai` flow

1. Resolve the server: `std::env::current_exe()`, canonicalized, with args
   `["mcp"]`.
2. Ask every tool, or the named ones, for its status, and print one aligned
   line per tool: `✓ claude-code  connected`, `○ cursor  not connected
   (~/.cursor/mcp.json)`, `· …  not installed`, or `! cursor  connected to
   /old/path/proxlet`.
3. Pick the targets: tools that are installed and either not connected or
   connected to another path. With none, print "Nothing to connect." and exit
   0.
4. Without `--yes`, ask once through `output::confirm`: "Connect proxlet to
   Cursor, Claude Code?". A declined answer exits 0. With no terminal, exit 1
   with the hint `proxlet ai --yes`.
5. Connect each target and print the result. One failure does not stop the
   others, but the exit status is 1 if any failed.

`proxlet ai rm [<tool>…]` disconnects every tool, or the named ones, and
prints each one's result. It does not ask, because removal only deletes
proxlet's own entry.

The CLI is built like `Cert`: `Ai { action: Option<AiAction::Rm { tools }>,
tools: Vec<String>, yes }` with `args_conflicts_with_subcommands`. `Mcp` is a
hidden subcommand, so it is not offered in completion or in `--help`. It is
documented for people who register it by hand. Tool names complete from the
static list `proxlet_ai::TOOL_IDS`.

### 6. Test isolation

- **Unit tests:** the `proxlet-ai` tests use a temporary `home` and a fake
  `CommandRunner` that records calls.
- **Integration tests** (`proxlet/tests/ai.rs`) run the real binary with
  `HOME` set to the test's directory and `PATH` set to a directory holding a
  fake `claude` script.
  - The script records its arguments to a file, and on `mcp add` and `mcp
    remove` it updates a `.claude.json` in that `HOME`, as the real CLI does.
  - The harness's `Home::command` gains an option to set `HOME` and `PATH`;
    today it leaves them alone.
- **MCP tests** (`proxlet/tests/mcp.rs`) spawn `proxlet mcp` with pipes,
  write JSON-RPC lines, and read the replies, against a real `proxlet run`
  where needed.

## Risks / Trade-offs

- **Claude Code's file layout changes:** a newer Claude Code could move
  user-scope servers. → Status would then read "not connected", and `connect`
  would run `remove` (which fails harmlessly) and then `add`. Registration
  itself goes through Claude Code's CLI, so it stays correct; only the shown
  status could be wrong.
- **A model makes unwanted changes:** an MCP client can add, remove and
  change. → This matches the user's choice of read-and-change powers. Every
  change is validated, reversible with the same tools, and recorded as an
  event in the logs.
  - The `logs` spec already lists configuration changes among event records,
    but the daemon does not record them today.
  - This change adds an info event when the daemon applies a rule or setting
    change, such as `rule added: /api → http://127.0.0.1:8000` or `setting
    changed: log.access off (was on)`, whether it came from the CLI or MCP. Clients ask the user before calling a tool that is not
  read-only, so the annotations matter. Certificates and the service stay
  CLI-only.
- **`current_exe` is a development build:** running `cargo run -- ai`
  registers `target/debug/proxlet`. → It works, and running `proxlet ai`
  again from the installed binary repairs it, as decision 5 shows.
- **Concurrent edits of `mcp.json`:** Cursor may write the file while proxlet
  does. → The window is tiny, because the file is read, changed in memory and
  renamed in one step. The backup allows recovery.
- **Unknown protocol versions:** negotiating down to the newest version
  proxlet knows is what the MCP specification prescribes. Clients that cannot
  accept it disconnect with their own error.

## Migration Plan

Nothing to migrate. `proxlet ai` only writes when the user confirms, and
`proxlet ai rm` removes everything it added. Rolling back the binary leaves
registrations pointing at a `proxlet` without `mcp`. The AI tool then reports
the server as failed, until `proxlet ai rm` or a reinstall.
