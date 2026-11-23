# proxlet

[![CI](https://github.com/VoIlAlex/proxlet/actions/workflows/ci.yml/badge.svg)](https://github.com/VoIlAlex/proxlet/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A tiny, fast reverse proxy and HTTP server written in Rust. One command to route
a path or a domain to a local port or a folder. It runs on its own, or attaches
to an nginx that already owns ports 80 and 443.

> **Status: early development.** The command surface below is settled; the
> behaviour behind it is being built change by change. See [Roadmap](#roadmap).

## What it looks like

```sh
proxlet start                      # install and start the system service

proxlet add /api 8000              # anything under /api      -> localhost:8000
proxlet add example.com/api 8002   # example.com/api          -> localhost:8002
proxlet add /files .               # serve this folder under /files

proxlet ls                         # list rules
proxlet rm /files                  # remove a rule
proxlet status                     # running? standalone or attached to nginx?

proxlet logs /api                  # follow logs for a route, a domain or a port
proxlet logs --all | grep 502      # dump everything for grep
proxlet monitor                    # live table: rps, errors, latency per rule
proxlet cert example.com           # Let's Encrypt certificate
proxlet ai                         # register the proxlet MCP server with your AI tools
proxlet config static.listing off  # change a setting
```

Every change is validated before it is written and applied, so a bad rule never
takes the server down. Everything on the command line tab-completes.

## How it works

- **Standalone:** proxlet listens on 80/443 itself.
- **Attached:** if nginx already holds those ports, proxlet installs a single
  catch-all in nginx that forwards otherwise-unclaimed traffic to proxlet.
  Routing, logs and stats stay in proxlet, so rules apply instantly without
  reloading nginx.
- If the ports are held by something that is not a supported server, proxlet
  refuses to start and tells you what is in the way.

## Install

No release yet. To build from source you need a stable Rust toolchain:

```sh
git clone https://github.com/VoIlAlex/proxlet
cd proxlet
cargo install --path crates/proxlet
```

Supported platforms: Linux and macOS.

## Roadmap

Work is specified with [OpenSpec](https://github.com/Fission-AI/OpenSpec) under
[`openspec/`](openspec/), one change per slice:

1. **Core** — rules, standalone serving (proxy + static files), `start`/`stop`/`status`, `config`
2. **Logs** — structured access and error logs, `proxlet logs`
3. **nginx attach** — detection, catch-all install, conflict warnings
4. **Certificates** — Let's Encrypt issuance and renewal, HTTPS
5. **Monitor** — live stats TUI
6. **AI tools** — MCP server and `proxlet ai`

## Project layout

| Crate | Purpose |
| --- | --- |
| `crates/proxlet` | The `proxlet` binary: CLI and daemon entry point |
| `crates/proxlet-core` | Rule model, matching, configuration |
| `crates/proxlet-server` | Proxy and static file engine |
| `crates/proxlet-front` | Front servers for attach mode (nginx first) |
| `crates/proxlet-service` | System service managers (systemd first) |
| `crates/proxlet-ai` | AI tool integrations |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
