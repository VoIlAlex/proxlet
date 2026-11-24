# Spec Delta

## Purpose

Connects proxlet's MCP server to the AI coding tools installed on the machine
with `proxlet ai`, and disconnects it with `proxlet ai rm`, without disturbing
the tools' other settings.

## ADDED Requirements

### Requirement: Detecting tools

`proxlet ai` SHALL list every supported tool (Claude Code, Cursor) with one of
three states: not installed; installed but not connected; or connected. A
registration that points at a different `proxlet` binary SHALL be shown as
connected to that other path.

#### Scenario: Overview

- **GIVEN** Claude Code is installed and connected, and Cursor is installed but not connected
- **WHEN** the user runs `proxlet ai --yes`
- **THEN** the output lists `claude-code` as connected and `cursor` as not connected, then connects Cursor

#### Scenario: Nothing installed

- **GIVEN** neither Claude Code nor Cursor is installed
- **WHEN** the user runs `proxlet ai`
- **THEN** it lists both as not installed, says there is nothing to connect, and exits 0

### Requirement: Connecting asks first

`proxlet ai [<tool>…]` SHALL ask once before connecting the installed tools
that are not connected, or are connected to another binary. With `--yes` it
SHALL not ask. Without a terminal and without `--yes`, it SHALL change nothing
and exit 1 with a hint to use `--yes`. Named tools SHALL limit it to those
tools.

#### Scenario: Confirmed

- **GIVEN** Cursor is installed but not connected
- **WHEN** the user runs `proxlet ai` in a terminal and answers yes
- **THEN** Cursor is connected, and the output says so

#### Scenario: Declined

- **WHEN** the user answers no
- **THEN** nothing changes and the command exits 0

#### Scenario: No terminal

- **WHEN** the user runs `proxlet ai | cat` with an unconnected tool installed
- **THEN** nothing changes, and the command exits 1 suggesting `proxlet ai --yes`

#### Scenario: One tool

- **GIVEN** Claude Code and Cursor are installed and neither is connected
- **WHEN** the user runs `proxlet ai cursor --yes`
- **THEN** only Cursor is connected

#### Scenario: Already connected

- **WHEN** every installed tool is already connected to this binary
- **THEN** `proxlet ai` says so, asks nothing, and exits 0

#### Scenario: Repairing a moved binary

- **GIVEN** Cursor's registration points at a `proxlet` that no longer exists
- **WHEN** the user runs `proxlet ai --yes`
- **THEN** the registration points at the running binary

### Requirement: Registration

Connecting SHALL register a server named `proxlet` at user scope that runs the
absolute path of the running binary with the argument `mcp`. For Claude Code
it SHALL use `claude mcp add --scope user`. For Cursor it SHALL set
`mcpServers.proxlet` in `~/.cursor/mcp.json`, creating the file if needed.

#### Scenario: Claude Code

- **WHEN** Claude Code is connected
- **THEN** `claude mcp add --scope user proxlet -- <path to proxlet> mcp` was run, and a failure of that command is reported with its output

#### Scenario: Cursor file created

- **GIVEN** `~/.cursor/` exists without `mcp.json`
- **WHEN** Cursor is connected
- **THEN** `~/.cursor/mcp.json` contains `{"mcpServers":{"proxlet":{"command":"<path to proxlet>","args":["mcp"]}}}`

### Requirement: Other settings are kept

When editing an existing tool configuration file, proxlet SHALL keep every
other entry and key unchanged and in order. It SHALL write a backup of the
previous file next to it (`<file>.proxlet-backup`) and replace the file
atomically. It SHALL refuse to edit a file it cannot parse, naming the file and
changing nothing.

#### Scenario: Existing servers kept

- **GIVEN** `~/.cursor/mcp.json` already registers a server `github`
- **WHEN** Cursor is connected
- **THEN** the `github` entry is unchanged, `proxlet` is added, and `~/.cursor/mcp.json.proxlet-backup` holds the previous file

#### Scenario: Unreadable file

- **GIVEN** `~/.cursor/mcp.json` is not valid JSON
- **WHEN** the user runs `proxlet ai cursor --yes`
- **THEN** the command exits 1 naming the file, and the file is unchanged

### Requirement: Disconnecting

`proxlet ai rm [<tool>…]` SHALL remove the `proxlet` registration from every
named tool, or from every tool where it is registered, and leave everything
else as it is.

#### Scenario: Remove everywhere

- **GIVEN** proxlet is connected to Claude Code and Cursor
- **WHEN** the user runs `proxlet ai rm`
- **THEN** `claude mcp remove --scope user proxlet` was run, `mcpServers.proxlet` is gone from `~/.cursor/mcp.json` with its other entries kept, and the output names both tools

#### Scenario: Not connected

- **WHEN** the user runs `proxlet ai rm cursor` and Cursor has no `proxlet` registration
- **THEN** it says Cursor was not connected and exits 0

### Requirement: Completion

`proxlet ai` and `proxlet ai rm` SHALL tab-complete their tool names
(`claude-code`, `cursor`) and flags.

#### Scenario: Complete a tool

- **WHEN** the user types `proxlet ai rm c` and presses Tab
- **THEN** `claude-code` and `cursor` are offered
