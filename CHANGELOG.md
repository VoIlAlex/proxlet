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

### Changed

- A port held by another process no longer stops `proxlet run` or `proxlet start`: proxlet logs the holder, waits, and takes the port when it is free. `proxlet status` shows `waiting for port 80 (held by …)`.
- `proxlet ls` plain output has a fourth tab-separated field (empty unless nginx serves the rule's host).
