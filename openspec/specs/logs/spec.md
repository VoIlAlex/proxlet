# logs Specification

## Purpose

Records what the server did with each request and what happened to the server
itself, keeps those records for a bounded time and size, and lets the user
read, filter and follow them with `proxlet logs`.

## Requirements

### Requirement: Access records

With `log.access` `on`, the server SHALL record every request it answers once
the response has been sent, or the connection ended. Each record SHALL hold the
time, client address, method, host, path without query, status, bytes sent,
duration, the matched rule (or none), the upstream address or served
directory, and the error reason for a proxy error. It SHALL NOT record
headers, query strings or bodies.

#### Scenario: Proxied request

- **GIVEN** a rule `/api` → port 8000
- **WHEN** a client at 203.0.113.7 requests `GET http://example.com/api/users?page=2` and the upstream answers 200 with 512 bytes
- **THEN** a record is written with method `GET`, host `example.com`, path `/api/users`, status 200, bytes 512, rule `/api`, upstream `127.0.0.1:8000` and client `203.0.113.7`, and it does not contain `page=2`

#### Scenario: Upstream down

- **GIVEN** a rule `/api` → port 8000 with nothing listening
- **WHEN** a request for `/api` gets 502
- **THEN** its record has status 502 and an error reason saying the connection was refused

#### Scenario: Streaming response

- **WHEN** an upstream streams a response over several seconds
- **THEN** one record is written when the response ends, with the total bytes and the full duration

#### Scenario: Behind nginx

- **GIVEN** proxlet is attached behind nginx
- **WHEN** a client at 203.0.113.7 makes a request that nginx forwards to proxlet
- **THEN** the record's client address is 203.0.113.7, not the loopback address

#### Scenario: Access logging off

- **GIVEN** `proxlet config log.access off`
- **WHEN** a client makes a request
- **THEN** no access record is written, while event records still are

### Requirement: Event records

The server SHALL record its own messages of level info and above, such as
listening, port handovers, attaching, configuration changes, and certificate
issuance and renewal, with their time, level and text. It SHALL keep printing
them to stderr as before.

#### Scenario: Port taken

- **GIVEN** another process holds the HTTP port
- **WHEN** the server starts
- **THEN** an event record with level `error` names the port and the holder

### Requirement: Log files and retention

Records SHALL be written as one JSON object per line to files in
`<state>/logs/`. That directory SHALL be readable only by the user proxlet runs
as. The current file SHALL be rotated at 10 MB, and the oldest files SHALL be
deleted while all files together exceed `log.max_size` or a file is older than
`log.max_age`. The current file SHALL never be deleted.

#### Scenario: Rotation and size limit

- **GIVEN** `log.max_size 30MB`
- **WHEN** the server has written 45 MB of records
- **THEN** `<state>/logs/` holds at most 30 MB of files, the newest records are kept, and no record was split across files

#### Scenario: Age limit

- **GIVEN** `log.max_age 1d` and a rotated log file last written two days ago
- **WHEN** the server next rotates or starts
- **THEN** that file is deleted

### Requirement: Logging never holds up requests

Writing records SHALL NOT block or fail any request. When records arrive faster
than they can be written, or the disk refuses writes, the server SHALL drop
records, count them, and once writing is possible again record an event saying
how many were dropped.

#### Scenario: Unwritable log directory

- **GIVEN** the log directory cannot be written
- **WHEN** clients make requests
- **THEN** every request is answered as usual and the server prints a warning to stderr

### Requirement: Reading logs

`proxlet logs` SHALL print the last 20 records and then follow new ones until
interrupted. `--all` SHALL print every stored record, oldest first, and exit.
It SHALL work whether or not the server is running. With no records it SHALL
say so and, unless following, exit zero.

#### Scenario: Follow

- **GIVEN** the server is running and has answered requests
- **WHEN** the user runs `proxlet logs` and a new request arrives
- **THEN** the last 20 records are printed, then the new request appears within a second

#### Scenario: All history

- **WHEN** the user runs `proxlet logs --all`
- **THEN** every stored record across all log files is printed oldest first and the command exits zero

#### Scenario: Rotation while following

- **GIVEN** `proxlet logs` is following
- **WHEN** the server rotates the log file
- **THEN** following continues with the new file without losing or repeating records

### Requirement: Selecting records

`proxlet logs <selector>` SHALL show only matching access records, and the event
records only when no selector is given. A selector starting with `/`, or a
`host/path`, SHALL match records whose rule is exactly that route; a number
SHALL match records whose upstream port is that number; anything else SHALL
match records whose host is that domain. A selector that is neither a valid
route, port nor domain SHALL exit non-zero.

#### Scenario: By route

- **GIVEN** rules `/api` and `/files`
- **WHEN** the user runs `proxlet logs /api --all`
- **THEN** only records whose rule is `/api` are printed

#### Scenario: By domain

- **WHEN** the user runs `proxlet logs example.com --all`
- **THEN** only records for host `example.com` are printed, whatever their rule

#### Scenario: By upstream port

- **WHEN** the user runs `proxlet logs 8000 --all`
- **THEN** only records proxied to an upstream on port 8000 are printed

### Requirement: Log output

In a terminal, each access record SHALL be one aligned line: time, status,
method, host and path, duration, bytes, and the upstream or directory, with
status colours for 4xx and 5xx. Event records SHALL show time, level and text.
When stdout is not a terminal the lines SHALL be the same without colour.
`--json` SHALL print the stored JSON lines unchanged.

#### Scenario: Piped

- **WHEN** the user runs `proxlet logs --all | grep " 502 "`
- **THEN** the 502 responses are printed, one per line, without escape codes

#### Scenario: JSON

- **WHEN** the user runs `proxlet logs --all --json`
- **THEN** each line is a JSON object with the fields of the stored record

### Requirement: Selector completion

Shell completion SHALL offer the routes of existing rules, the domains of host
rules, and the ports of proxy upstreams after `proxlet logs`.

#### Scenario: Complete a selector

- **GIVEN** rules `/api` → 8000 and `example.com/` → 3000
- **WHEN** the user presses TAB after `proxlet logs `
- **THEN** `/api`, `example.com/`, `example.com`, `8000` and `3000` are offered
