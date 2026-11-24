# Spec Delta

## Purpose

Lets AI coding tools inspect and change proxlet through the Model Context
Protocol, with the same validation and safety as the CLI, by running
`proxlet mcp` as a stdio server.

## ADDED Requirements

### Requirement: MCP over stdio

`proxlet mcp` SHALL speak MCP (JSON-RPC 2.0, one message per line) on stdin
and stdout, and write nothing else to stdout. It SHALL answer `initialize`
with its name, version and tools capability, `ping`, `tools/list` and
`tools/call`; answer unknown methods with JSON-RPC error -32601; and exit 0
when stdin closes.

#### Scenario: Handshake

- **WHEN** a client sends `initialize` with protocol version `2025-06-18`
- **THEN** the reply has `protocolVersion` `2025-06-18`, `serverInfo.name` `proxlet`, and a `tools` capability

#### Scenario: Unknown protocol version

- **WHEN** a client sends `initialize` with a protocol version proxlet does not know
- **THEN** the reply names the newest version proxlet supports

#### Scenario: Unknown method

- **WHEN** a client sends a request with method `resources/list`
- **THEN** the reply is a JSON-RPC error with code -32601, and the server keeps running

#### Scenario: Malformed input

- **WHEN** a client sends a line that is not JSON
- **THEN** the reply is a JSON-RPC parse error (-32700), and the server keeps running

#### Scenario: Client goes away

- **WHEN** stdin closes
- **THEN** `proxlet mcp` exits with status 0

### Requirement: Tools offered

`tools/list` SHALL offer exactly these tools, each with a description and a
JSON Schema for its arguments: `status`, `list_rules`, `get_settings`,
`read_logs`, `get_stats`, `add_rule`, `remove_rule` and `set_setting`. It
SHALL NOT offer tools for certificates or for starting or stopping the
service.

#### Scenario: Listing tools

- **WHEN** a client calls `tools/list`
- **THEN** it receives the eight tools, and the read-only ones are annotated as read-only

### Requirement: Reading state

The read tools SHALL return JSON text:
- `status`: whether the server runs, and when it does, what `proxlet status` reports.
- `list_rules`: every rule with route, kind and target.
- `get_settings`: every setting with value, default and description.
- `read_logs`: up to `limit` most recent records (default 50, at most 500), oldest first, optionally filtered by a `proxlet logs` selector.
- `get_stats`: the live statistics `proxlet monitor` shows.

#### Scenario: Rules

- **GIVEN** rules `/api` → port 8000 and `/docs` → `/srv/docs`
- **WHEN** a client calls `list_rules`
- **THEN** the result lists both rules with their routes, kinds (`proxy`, `files`) and targets

#### Scenario: Logs for a route

- **GIVEN** stored records for `/api` and `/docs`
- **WHEN** a client calls `read_logs` with selector `/api` and limit 5
- **THEN** the result holds at most 5 records, all answered by `/api`, oldest first, in the stored JSON form

#### Scenario: Bad selector

- **WHEN** a client calls `read_logs` with a selector `proxlet logs` rejects
- **THEN** the result is a tool error with the same message `proxlet logs` prints

#### Scenario: Not running

- **GIVEN** proxlet is not running
- **WHEN** a client calls `status`, `list_rules`, `get_settings` and `read_logs`
- **THEN** `status` says it is not running and the others answer from the configuration and log files

#### Scenario: Stats need the server

- **GIVEN** proxlet is not running
- **WHEN** a client calls `get_stats`
- **THEN** the result is a tool error saying proxlet is not running and how to start it

### Requirement: Changing rules and settings

`add_rule`, `remove_rule` and `set_setting` SHALL apply exactly as `proxlet
add`, `proxlet rm` and `proxlet config KEY VALUE` do: validated first, written
atomically, rolled back if applying fails, and applied live when the server
runs. Each SHALL return what changed, including the previous value, and
whether the change is live.

#### Scenario: Add a rule

- **GIVEN** proxlet is running
- **WHEN** a client calls `add_rule` with route `/api` and target `8000`
- **THEN** the rule is saved and served at once, and the result says it was added and is live

#### Scenario: Invalid change

- **WHEN** a client calls `set_setting` with key `log.max_size` and value `1MB`
- **THEN** the result is a tool error with the CLI's message, and nothing is written

#### Scenario: Remove a missing rule

- **WHEN** a client calls `remove_rule` for a route that has no rule
- **THEN** the result is a tool error saying there is no rule for that route

#### Scenario: Server stopped

- **GIVEN** proxlet is not running
- **WHEN** a client calls `add_rule`
- **THEN** the configuration is updated and the result says it applies when proxlet starts

### Requirement: Directory targets are absolute

`add_rule` SHALL reject a directory target that is not an absolute path,
saying an absolute path is needed. It SHALL validate the directory as `proxlet
add` does.

#### Scenario: Relative directory

- **WHEN** a client calls `add_rule` with route `/docs` and target `./site`
- **THEN** the result is a tool error asking for an absolute path, and nothing is written

### Requirement: Hosts the front server serves

When proxlet is attached and the front server already serves the rule's host,
`add_rule` SHALL refuse with the CLI's warning unless the call sets
`allow_front_conflict` to true.

#### Scenario: Host served by nginx

- **GIVEN** proxlet is attached behind nginx, which serves `site.example`
- **WHEN** a client calls `add_rule` with route `site.example/` and target `3000`
- **THEN** the result is a tool error saying nginx serves `site.example` and that `allow_front_conflict` adds it anyway, and nothing is written

#### Scenario: Added anyway

- **GIVEN** the same setup
- **WHEN** the client calls `add_rule` again with `allow_front_conflict` true
- **THEN** the rule is added
