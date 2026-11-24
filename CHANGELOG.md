# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Rules: `proxlet add`, `rm` and `ls`, for routes (`/path`, `host/path`) to ports, `host:port`, URLs or directories.
- Reverse proxy with streamed bodies, forwarding headers, WebSocket upgrades, and 502/504 errors that name the route and upstream.
- Static file serving with content types, conditional and range requests, index files, directory listings, and protection against traversal, symlink escapes and dotfiles.
- `proxlet run` with a port check, live rule and setting changes, and graceful shutdown.
- `sudo proxlet start` / `proxlet stop` as a systemd service that runs as the invoking user.
- `proxlet status` and `proxlet config`.
- Tab completion for commands, rules, setting names and values.
- Attach behind nginx: proxlet hands port 80 to nginx when nginx starts (systemd hook, or by following nginx's error log), takes it back when nginx stops, and installs one catch-all server block so nginx forwards every host it does not serve. `sudo proxlet stop` restores nginx's config exactly.
- Taking over nginx's default server after confirmation (`sudo proxlet start --yes` to skip the prompt).
- `proxlet add` warns and asks when nginx already serves the host; `proxlet ls` marks such rules.
- Settings `attach.enabled` and `attach.port`.
- `proxlet cert <domain>…` gets Let's Encrypt certificates through the HTTP-01 challenge, which the running server answers. It first checks that the name is public, resolves, is not served by nginx, and reaches this proxlet; `--yes` accepts the CA's terms. No email address is asked for.
- `proxlet cert ls`, `cert rm` and `cert renew`; automatic renewal when a third of a certificate's lifetime is left, with backoff on failure. `proxlet status` warns about expiring or failing certificates.
- HTTPS: proxlet serves port 443 (`listen.https_port`) with SNI certificate selection, HTTP/2 and WebSockets; certificates change without a restart. Port 443 follows the same ownership rules as port 80.
- HTTPS behind nginx: a second catch-all on port 443 that terminates TLS with proxlet's certificate for the requested name (nginx 1.15.9 or newer). The default-server takeover covers both ports.
- Settings `listen.https_port`, `tls.redirect` and `cert.server` (`production`, `staging` or an ACME directory URL).
- Request logs: every request is recorded (client, method, host, path without the query, status, bytes, duration, rule, upstream or directory, and the proxy error), along with the server's own messages, as JSON lines in `<state>/logs/`. Files rotate at 10 MB and are kept within `log.max_size` (`100MB`) and `log.max_age` (`7d`); recording never holds up a request.
- `proxlet logs [<selector>]` shows the last 20 records and follows new ones; `--all` prints every stored record, `--json` the stored lines. A selector is a route, a domain or an upstream port, and tab-completes. It works while proxlet is stopped.
- Settings `log.access`, `log.max_size` and `log.max_age`. Durations accept `h` and `d`; sizes are written `KB`, `MB` or `GB`.

### Changed

- **Breaking:** once a host has a certificate, plain HTTP requests for it are redirected to HTTPS (`308`) while HTTPS is served. `proxlet config tls.redirect off` keeps the old behaviour.
- Declining the default-server takeover now keeps nginx's default only on the ports that have one; a port without a default server still gets proxlet's block.
- The HTTP, HTTPS and attach ports must differ; `proxlet config` refuses a change that would make two of them equal.

- A port held by another process no longer stops `proxlet run` or `proxlet start`: proxlet logs the holder, waits, and takes the port when it is free. `proxlet status` shows `waiting for port 80 (held by …)`.
- `proxlet ls` plain output has a fourth tab-separated field (empty unless nginx serves the rule's host).
