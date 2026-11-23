# Contributing to proxlet

Thanks for your interest. Bug reports, ideas and pull requests are all welcome.

## Getting started

You need a stable Rust toolchain (`rustup` picks it up from `rust-toolchain.toml`).

```sh
cargo build
cargo test --workspace
```

Before opening a pull request, make sure these pass — CI runs the same three:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## How changes are planned

Behaviour changes are specified with [OpenSpec](https://github.com/Fission-AI/OpenSpec)
before they are implemented:

- `openspec/specs/` describes what proxlet does today.
- `openspec/changes/<name>/` holds a proposed change: `proposal.md` (why),
  `specs/` (what), `design.md` (how) and `tasks.md` (steps).

For anything beyond a small fix, open an issue or a change proposal first so the
approach can be agreed before you write the code. Run `openspec validate <name>`
to check a proposal.

## Extending proxlet

Three areas are built to be extended, each behind one trait in its own crate:

- **Front servers** (`crates/proxlet-front`) — nginx today; Apache, Caddy, ...
- **Service managers** (`crates/proxlet-service`) — systemd today; launchd, OpenRC, ...
- **AI tools** (`crates/proxlet-ai`) — one module per tool.

A new integration should be a new module implementing the trait, with no changes
to the engine or to other integrations. [docs/extending.md](docs/extending.md)
walks through adding one, step by step.

## Pull requests

- Keep each pull request to one logical change.
- Add tests for new behaviour and for fixed bugs.
- Update the docs and `CHANGELOG.md` when user-visible behaviour changes.

## License

By contributing you agree that your contributions are dual licensed under the
MIT and Apache-2.0 licenses, as described in the README.
