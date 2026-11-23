# Proposal

## Why

proxlet has a command surface but no behaviour. This change delivers the first
usable slice: a standalone server whose routing is managed entirely with
`proxlet add` / `rm` / `ls`, so a developer can put several local services and
folders behind one port in seconds.

## What Changes

- Add the rule model: a route (`/path` or `host/path`) mapped to a target (a
  local port, a `host:port` / URL upstream, or a directory).
- Add `proxlet add`, `proxlet rm` and `proxlet ls`, with validation before
  anything is written and live apply when the server is running.
- Add the standalone HTTP engine: reverse proxying (including WebSocket
  upgrades and streaming) and static file serving.
- Add `proxlet start` / `proxlet stop` (system service, systemd first, behind a
  service-manager abstraction), `proxlet run` (foreground) and `proxlet status`.
- Add the startup port check: refuse to start when the HTTP port is already
  taken, naming the process that holds it.
- Add `proxlet config` to list, read and change settings, starting with
  directory listings (on by default).
- Tab-completion of rules and setting names, on top of the existing command
  completion.

Out of scope, each owned by a later change:

- Structured access/error logs and `proxlet logs` — *logs* change.
- nginx detection, attach mode and conflict warnings in `add` / `ls` —
  *nginx attach* change. Until then any occupied port is an error.
- HTTPS, port 443 and `proxlet cert` — *certificates* change.
- `proxlet monitor` and traffic stats — *monitor* change.
- `proxlet ai` and the MCP server — *AI tools* change.
- Wildcard hosts, load balancing across several upstreams, header rewriting.

## Capabilities

### New Capabilities

- `proxy-rules`: what a rule is, how routes and targets are written, how rules
  are added, replaced, removed and listed, and how they are validated and persisted.
- `http-serving`: how an incoming request is matched to a rule and served,
  for both proxied upstreams and static directories, including error responses.
- `daemon-lifecycle`: starting, stopping and inspecting the server, the system
  service integration, and the startup port check.
- `settings`: the `proxlet config` command and the settings it manages.

### Modified Capabilities

None.

## Impact

- Code: `proxlet-core` (rules, matching, config file), `proxlet-server`
  (engine), `proxlet-service` (service-manager trait + systemd), `proxlet`
  (CLI, control client, completions).
- New dependencies: an async runtime, an HTTP library, a TOML reader/writer and
  a serialisation library (choices in `design.md`).
- On disk: a configuration file, a control socket and, on Linux, a systemd unit.
- No existing behaviour changes; every command currently exits "not implemented".
