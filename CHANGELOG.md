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
