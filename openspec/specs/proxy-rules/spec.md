# proxy-rules Specification

## Purpose

Defines what a proxlet rule is, how routes and targets are written on the command
line, and how rules are added, replaced, removed, listed, validated and persisted.

## Requirements


### Requirement: Route syntax

A route SHALL be written as `/path` (matches any host) or `host/path` (matches
one host). A bare `host` SHALL mean `host/`. Hosts SHALL be compared
case-insensitively and stored in lower case; a trailing slash on the path SHALL
be ignored except for the root path `/`.

#### Scenario: Path-only route

- **WHEN** the user runs `proxlet add /api 8000`
- **THEN** a rule is stored with no host and path `/api`

#### Scenario: Host route

- **WHEN** the user runs `proxlet add Example.com/api/ 8002`
- **THEN** a rule is stored with host `example.com` and path `/api`

#### Scenario: Bare host

- **WHEN** the user runs `proxlet add example.com 8002`
- **THEN** a rule is stored with host `example.com` and path `/`

#### Scenario: Invalid route

- **WHEN** the user runs `proxlet add "exa mple.com/api" 8000`
- **THEN** the command exits non-zero, explains that the route is not valid, and stores nothing

### Requirement: Target syntax

A target SHALL be interpreted in this order: an integer from 1 to 65535 is a port
on the local machine; a `host:port` pair or an `http://` / `https://` URL is an
upstream; anything else is a directory path. A directory target SHALL be stored
as an absolute path resolved at the time the rule is added.

#### Scenario: Port target

- **WHEN** the user runs `proxlet add /api 8000`
- **THEN** the rule's target is the upstream `http://127.0.0.1:8000`

#### Scenario: Upstream target

- **WHEN** the user runs `proxlet add /api 10.0.0.5:9000`
- **THEN** the rule's target is the upstream `http://10.0.0.5:9000`

#### Scenario: Directory target

- **WHEN** the user runs `proxlet add /files .` from `/home/me/site`
- **THEN** the rule's target is the directory `/home/me/site`

#### Scenario: Directory whose name is a number

- **WHEN** the user runs `proxlet add /files ./8000` and `./8000` is a directory
- **THEN** the rule's target is that directory, not port 8000

### Requirement: Validation before apply

`proxlet add` SHALL validate the rule before changing anything. A directory target
that does not exist or is not a readable directory, an invalid route, or an
out-of-range port SHALL be rejected with a non-zero exit and no change. A port
target that nothing is listening on SHALL be accepted with a warning.

#### Scenario: Missing directory

- **WHEN** the user runs `proxlet add /files ./nope` and `./nope` does not exist
- **THEN** the command exits non-zero, names the missing path, and the rule set is unchanged

#### Scenario: Upstream not listening yet

- **WHEN** the user runs `proxlet add /api 8000` and nothing listens on port 8000
- **THEN** the rule is added and the output warns that nothing is listening on 8000

### Requirement: Atomic persist and live apply

A rule change SHALL be persisted so it survives a restart, and SHALL take effect
on a running server without a restart and without interrupting in-flight
requests. If the running server rejects the change, the persisted rule set SHALL
be left as it was before the command and the command SHALL exit non-zero.

#### Scenario: Add while running

- **WHEN** the server is running and the user runs `proxlet add /api 8000`
- **THEN** the next request to `/api` is served by the new rule, and the command prints a confirmation and exits zero

#### Scenario: Add while stopped

- **WHEN** the server is not running and the user runs `proxlet add /api 8000`
- **THEN** the rule is saved, the output says it will apply when proxlet starts, and the command exits zero

#### Scenario: Persistence across restart

- **WHEN** a rule has been added and the server is restarted
- **THEN** the rule is still listed by `proxlet ls` and still serves requests

### Requirement: Replacing a rule

A rule SHALL be identified by its host and path. Adding a rule whose host and
path already exist SHALL replace that rule's target, and the output SHALL say the
rule was updated and show the previous target.

#### Scenario: Update target

- **WHEN** `/api` points to port 8000 and the user runs `proxlet add /api 9000`
- **THEN** `/api` points to port 9000 and the output states it was updated from 8000

### Requirement: Removing a rule

`proxlet rm <route>` SHALL remove the rule with exactly that host and path, with
the same validation, persistence and live-apply guarantees as `add`. Removing a
route that has no rule SHALL exit non-zero and change nothing.

#### Scenario: Remove existing rule

- **WHEN** a rule exists for `example.com/api` and the user runs `proxlet rm example.com/api`
- **THEN** the rule is removed and requests it used to match are no longer served by it

#### Scenario: Remove unknown rule

- **WHEN** no rule exists for `/nope` and the user runs `proxlet rm /nope`
- **THEN** the command exits non-zero and says no such rule exists

### Requirement: Listing rules

`proxlet ls` SHALL list every rule with its route, its target and the kind of
target (proxy or files). On a terminal the output SHALL be an aligned table; when
stdout is not a terminal it SHALL be one rule per line with tab-separated fields
and no decoration. With no rules it SHALL say so and exit zero.

#### Scenario: List rules

- **WHEN** rules exist for `/api` → port 8000 and `/files` → `/home/me/site` and the user runs `proxlet ls`
- **THEN** both rules are shown with route, kind and target

#### Scenario: Empty list

- **WHEN** no rules exist and the user runs `proxlet ls`
- **THEN** the output says there are no rules and suggests `proxlet add`, and the command exits zero

### Requirement: Rule completion

Shell completion SHALL offer the routes of existing rules as values for
`proxlet rm`.

#### Scenario: Complete rm

- **WHEN** rules exist for `/api` and `example.com/api` and the user presses TAB after `proxlet rm `
- **THEN** `/api` and `example.com/api` are offered
