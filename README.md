# proxlet

[![CI](https://github.com/VoIlAlex/proxlet/actions/workflows/ci.yml/badge.svg)](https://github.com/VoIlAlex/proxlet/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A tiny, fast reverse proxy and HTTP server written in Rust. One command to route
a path or a domain to a local port or a folder. It runs on its own, or attaches
to an nginx that already owns ports 80 and 443.

> **Status: early development.** Rules, proxying, static files, settings, the
> system service, attaching behind nginx, Let's Encrypt certificates with
> HTTPS, request logs, the live monitor, and the MCP server for AI tools work
> today. See [Roadmap](#roadmap).

## What it looks like

```sh
proxlet start                      # install and start the system service

proxlet add /api 8000              # anything under /api      -> localhost:8000
proxlet add example.com/api 8002   # example.com/api          -> localhost:8002
proxlet add /files .               # serve this folder under /files

proxlet ls                         # list rules
proxlet rm /files                  # remove a rule
proxlet status                     # running? standalone or attached to nginx?

proxlet logs /api                  # follow requests for a route, a domain or a port
proxlet logs --all | grep " 502 "  # every stored record, for grep
proxlet monitor                    # live table: rps, errors, latency per rule
proxlet cert example.com           # Let's Encrypt certificate, HTTPS, auto-renewed
proxlet ai                         # register the proxlet MCP server with your AI tools
proxlet config static.listing off  # change a setting
```

Every change is validated before it is written and applied, so a bad rule never
takes the server down. Everything on the command line tab-completes.

**[Read the usage guide →](docs/usage.md)** for routes, targets, matching,
settings, logs and where proxlet keeps its files.

## How it works

- **Standalone:** proxlet listens on ports 80 and 443 itself.
- **Attached:** if nginx holds the port, or starts later, proxlet hands it over
  and installs a single catch-all in nginx that forwards otherwise-unclaimed
  traffic to proxlet. When nginx stops, proxlet takes the port back. Routing,
  logs and stats stay in proxlet, so rules apply instantly without reloading
  nginx. HTTPS works the same way: nginx terminates TLS for unclaimed names
  with proxlet's own certificates, read per connection, so a new certificate
  needs no nginx reload. `sudo proxlet stop` restores nginx's config exactly.
- **Certificates:** `proxlet cert example.com` checks the name, then gets a
  certificate from Let's Encrypt, answering the challenge itself. Renewal is
  automatic; plain HTTP redirects to HTTPS for hosts that have one.
- If the port is held by something that is not a supported server, proxlet
  says what is in the way and takes the port as soon as it is free.

## Install

No release yet. To build from source you need a stable Rust toolchain:

```sh
git clone https://github.com/VoIlAlex/proxlet
cd proxlet
cargo install --path crates/proxlet
```

Supported platforms: Linux and macOS.

Try it without touching port 80:

```sh
proxlet config listen.http_port 8080
proxlet add /files .
proxlet run          # then open http://localhost:8080/files/
```

Enable tab completion (zsh shown; bash and fish are in the [usage guide](docs/usage.md#shell-completion)):

```sh
echo 'source <(COMPLETE=zsh proxlet)' >> ~/.zshrc
```

## Roadmap

Work is specified with [OpenSpec](https://github.com/Fission-AI/OpenSpec) under
[`openspec/`](openspec/), one change per slice:

1. **Core** — rules, standalone serving (proxy + static files), `start`/`stop`/`status`, `config` *(done)*
2. **nginx attach** — port handoff, catch-all install, conflict warnings *(done)*
3. **Certificates** — Let's Encrypt issuance and renewal, HTTPS *(done)*
4. **Logs** — structured access and error logs, `proxlet logs` *(done)*
5. **Monitor** — live stats TUI *(done)*
6. **AI tools** — MCP server and `proxlet ai` *(done)*

## Project layout

| Crate | Purpose |
| --- | --- |
| `crates/proxlet` | The `proxlet` binary: CLI and daemon entry point |
| `crates/proxlet-core` | Rule model, matching, configuration |
| `crates/proxlet-server` | Proxy and static file engine |
| `crates/proxlet-front` | Front servers for attach mode (nginx first) |
| `crates/proxlet-cert` | Certificates: ACME issuance, storage, renewal |
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
