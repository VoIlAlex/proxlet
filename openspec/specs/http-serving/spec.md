# http-serving Specification

## Purpose

Defines how proxlet matches an incoming HTTP request to a rule and serves it,
either by proxying to an upstream or by serving files from a directory.

## Requirements


### Requirement: Rule matching

A request SHALL be matched by host first, then by path. Rules for the request's
exact host are considered before rules with no host. Within a group the rule with
the longest matching path wins. A path matches only on a segment boundary: `/api`
matches `/api` and `/api/users` but not `/apix`.

#### Scenario: Longest path wins

- **WHEN** rules exist for `/` and `/api` and a request arrives for `/api/users`
- **THEN** the `/api` rule serves it

#### Scenario: Host rule beats hostless rule

- **WHEN** rules exist for `/api` and `example.com/` and a request arrives for `example.com/api/users`
- **THEN** the `example.com/` rule serves it

#### Scenario: Fallback to hostless rule

- **WHEN** rules exist for `/api` and `example.com/admin` and a request arrives for `example.com/api`
- **THEN** the `/api` rule serves it

#### Scenario: Segment boundary

- **WHEN** only `/api` exists and a request arrives for `/apix`
- **THEN** no rule matches

### Requirement: No matching rule

A request that matches no rule SHALL receive a `404` response whose body says
that proxlet has no rule for that host and path.

#### Scenario: Unmatched request

- **WHEN** no rule matches a request for `example.com/nothing`
- **THEN** the response status is 404 and the body identifies proxlet as the responder

### Requirement: Proxying to an upstream

A request matched by a proxy rule SHALL be forwarded to the rule's upstream with
its path and query string unchanged, including the rule's path prefix. Request
and response bodies SHALL be streamed, not buffered in full.

#### Scenario: Path is preserved

- **WHEN** `/api` points to port 8000 and a request arrives for `/api/users?page=2`
- **THEN** the upstream on port 8000 receives a request for `/api/users?page=2`

#### Scenario: Streaming response

- **WHEN** an upstream sends a response in chunks over several seconds
- **THEN** the client receives each chunk as it is sent, without waiting for the response to finish

### Requirement: Forwarding headers

A proxied request SHALL keep the client's original `Host` header and SHALL carry
`X-Forwarded-For` (client address appended to any existing value),
`X-Forwarded-Proto` and `X-Forwarded-Host`. Hop-by-hop headers SHALL NOT be
forwarded in either direction.

#### Scenario: Forwarded headers present

- **WHEN** a client at 203.0.113.7 requests `http://example.com/api` and it is proxied
- **THEN** the upstream sees `Host: example.com`, `X-Forwarded-For: 203.0.113.7`, `X-Forwarded-Proto: http` and `X-Forwarded-Host: example.com`

### Requirement: Protocol upgrades

A proxied request that asks for a protocol upgrade (such as WebSocket) SHALL be
upgraded end to end when the upstream accepts it, and data SHALL then flow in
both directions until either side closes.

#### Scenario: WebSocket through proxlet

- **WHEN** a client opens a WebSocket to a route proxied to a WebSocket server
- **THEN** the handshake succeeds and messages pass in both directions

### Requirement: Upstream failures

When the upstream cannot be reached proxlet SHALL respond `502`, and when it does
not respond within the configured time proxlet SHALL respond `504`. The body
SHALL name the route and the upstream so the cause is obvious to a developer.

#### Scenario: Upstream down

- **WHEN** `/api` points to port 8000, nothing listens there, and a request arrives for `/api`
- **THEN** the response status is 502 and the body says `/api` could not reach `127.0.0.1:8000`

#### Scenario: Upstream too slow

- **WHEN** an upstream accepts a request but sends no response within the configured timeout
- **THEN** the response status is 504

### Requirement: Serving files

A request matched by a files rule SHALL be served from the rule's directory, with
the rule's path prefix removed before the file is looked up. Responses SHALL
carry a content type derived from the file name and SHALL support conditional
and range requests. A path with no file SHALL receive `404`.

#### Scenario: File under a prefix

- **WHEN** `/files` points to `/home/me/site` and a request arrives for `/files/img/logo.png`
- **THEN** `/home/me/site/img/logo.png` is returned with an image content type

#### Scenario: Missing file

- **WHEN** a request arrives for a path with no corresponding file
- **THEN** the response status is 404

#### Scenario: Range request

- **WHEN** a client requests a byte range of an existing file
- **THEN** the response status is 206 and only that range is returned

### Requirement: Directories and index files

A request for a directory SHALL return its `index.html` when one exists.
Otherwise it SHALL return a listing of the directory when the `static.listing`
setting is on, and `404` when it is off.

#### Scenario: Index file

- **WHEN** a request arrives for a directory that contains `index.html`
- **THEN** that file is returned

#### Scenario: Listing on

- **WHEN** `static.listing` is on and a request arrives for a directory without `index.html`
- **THEN** an HTML listing of the directory's entries is returned

#### Scenario: Listing off

- **WHEN** `static.listing` is off and a request arrives for a directory without `index.html`
- **THEN** the response status is 404

#### Scenario: Directory without a trailing slash

- **WHEN** a request arrives for `/files/docs?x=1` and `docs` is a directory
- **THEN** the response is a 301 redirect to `/files/docs/?x=1`, so relative links in the page resolve

### Requirement: Staying inside the served directory

A files rule SHALL never serve content from outside its directory, whether
reached by `..` segments, encoded separators or a symbolic link that leaves the
directory. Files and directories whose name starts with a dot SHALL be neither
served nor listed unless the `static.dotfiles` setting is on.

#### Scenario: Path traversal

- **WHEN** a request arrives for `/files/../../etc/passwd` or an encoded equivalent
- **THEN** the response status is 404 and nothing outside the directory is read

#### Scenario: Dotfile hidden by default

- **WHEN** `static.dotfiles` is off and a request arrives for `/files/.env`
- **THEN** the response status is 404 even though the file exists
