# monitor Specification

## Purpose

Keeps live traffic statistics for each rule in the running server and shows
them with `proxlet monitor`, so the user can see at a glance which route is
busy, failing or slow.

## Requirements

### Requirement: Statistics per rule

The running server SHALL keep statistics for each current rule, and one
"no rule" row for requests no rule answered (no match, HTTPS redirects, ACME
challenges). It SHALL keep them whether or not `log.access` is on, in memory
only, counted from when the server started. A request SHALL count once its
response status is known.

#### Scenario: Requests counted per rule

- **GIVEN** rules `/api` → port 8000 and `/` → port 3000
- **WHEN** a client makes 3 requests for `/api/users` and 2 for `/index.html`
- **THEN** the `/api` row has 3 total requests and the `/` row has 2

#### Scenario: Unmatched requests

- **GIVEN** only a rule `example.com/` → port 3000
- **WHEN** a client requests `http://other.example/`
- **THEN** the "no rule" row has 1 request with a 4xx response, and the `example.com/` row has none

#### Scenario: Access logging off

- **GIVEN** `proxlet config log.access off`
- **WHEN** a client requests a route
- **THEN** the request is still counted in its rule's row

#### Scenario: Restart

- **WHEN** the server restarts
- **THEN** every total starts again from 0

### Requirement: Rule rows follow the rules

The statistics SHALL list exactly the current rules plus the "no rule" row,
including rules that have had no traffic. A removed rule's row SHALL
disappear, and a rule added again SHALL start from zero.

#### Scenario: New rule

- **WHEN** the user runs `proxlet add /docs ./site` while the server runs
- **THEN** the next snapshot has a `/docs` row with 0 total requests

#### Scenario: Removed rule

- **GIVEN** a rule `/api` that has served requests
- **WHEN** the user removes it and then adds `/api` again
- **THEN** the `/api` row has 0 total requests

#### Scenario: Target changed

- **GIVEN** a rule `/api` → port 8000 that has served requests
- **WHEN** the user points `/api` at port 9000
- **THEN** the `/api` row keeps its totals and shows the new target

### Requirement: What each row shows

Each row SHALL show the rule's route and target; requests per second over the
last 10 seconds; total requests; 4xx and 5xx responses; upstream errors (502
or 504 from proxlet failing to reach the upstream); p50, p95 and p99 latency
over the last 60 seconds; bytes sent per second over the last 10 seconds; and
requests in flight and open WebSocket tunnels.

#### Scenario: Errors

- **GIVEN** a rule `/api` → port 8000 with nothing listening
- **WHEN** a client makes 4 requests for `/api`
- **THEN** the `/api` row shows 4 total, 4 under 5xx and 4 upstream errors

#### Scenario: Upstream's own 5xx

- **GIVEN** an upstream that answers 500
- **WHEN** a client requests its route
- **THEN** the row counts one 5xx and no upstream error

#### Scenario: Latency

- **GIVEN** an upstream that answers after 200 ms
- **WHEN** a client makes 10 requests for its route within a minute
- **THEN** the row's p50, p95 and p99 are each between 180 ms and 260 ms

#### Scenario: Latency is time to the response head

- **GIVEN** an upstream that answers at once and then streams its body for 5 seconds
- **WHEN** a client requests its route
- **THEN** the request's latency is under 100 ms, and bytes per second are above 0 while the body streams

#### Scenario: No recent traffic

- **WHEN** a rule has had no requests in the last 60 seconds
- **THEN** its latency columns show `-`, its rates show 0, and its totals keep their values

#### Scenario: WebSocket open

- **GIVEN** a rule `/ws` → a WebSocket upstream
- **WHEN** a client opens a WebSocket through `/ws` and keeps it open
- **THEN** the `/ws` row shows 1 open WebSocket, and 0 once the client closes it

#### Scenario: Request in flight

- **GIVEN** an upstream that waits 3 seconds before answering
- **WHEN** a client has a request to it waiting
- **THEN** its row shows 1 request in flight, and 0 after the response ends

### Requirement: Statistics never slow requests

Recording statistics SHALL NOT block, take a lock, or allocate memory per
request, and the memory used SHALL be bounded per rule regardless of traffic.

#### Scenario: Heavy traffic

- **WHEN** clients send many thousands of requests per second
- **THEN** no request waits on the statistics, and the server's memory does not grow with the number of requests

### Requirement: Reading statistics over the control socket

The control socket SHALL answer a `stats` request with a snapshot: the server's
uptime, the number of open client connections, and every row with the values
above.

#### Scenario: Snapshot

- **WHEN** a client sends `{"cmd":"stats"}` on the control socket
- **THEN** the reply has `"ok":true` and a `stats` object with `uptime_secs`, `connections` and one entry in `rows` per rule plus the "no rule" row

### Requirement: Live table

In a terminal, `proxlet monitor` SHALL show a full-screen table with one row
per rule and the "no rule" row last, refreshed every second, with the uptime
and open connections above it. `s` SHALL change the sort column (route,
requests per second, errors, p95), the arrow keys SHALL scroll, and `q`, `Esc`
or Ctrl-C SHALL restore the terminal and exit 0.

#### Scenario: Watching traffic

- **GIVEN** the server is running with a rule `/api`
- **WHEN** the user runs `proxlet monitor` and makes requests to `/api`
- **THEN** within 2 seconds the `/api` row shows the new requests

#### Scenario: Sorting

- **WHEN** the user presses `s` in `proxlet monitor`
- **THEN** the rows are sorted by the next column, and the header marks it

#### Scenario: Quit

- **WHEN** the user presses `q`
- **THEN** the terminal is restored as it was and the command exits with status 0

### Requirement: Server stops while watching

If the server stops while `proxlet monitor` is showing the table, it SHALL say
that the server is not running and keep trying each second; it SHALL show the
table again once the server answers.

#### Scenario: Restart while watching

- **GIVEN** `proxlet monitor` is open
- **WHEN** the user runs `proxlet stop` and then `proxlet start`
- **THEN** the table says the server is not running, then shows rows again with totals from 0

### Requirement: One snapshot

When stdout is not a terminal, or with `--once`, `proxlet monitor` SHALL print
one header line and one plain aligned line per row, without colour or
terminal control sequences, and exit 0.

#### Scenario: Piped

- **GIVEN** the server is running with rules `/api` and `/`
- **WHEN** the user runs `proxlet monitor | cat`
- **THEN** the output is a header line starting with `RULE`, then lines for `/`, `/api` and `(no rule)`, with no escape sequences, and the exit status is 0

#### Scenario: Once in a terminal

- **WHEN** the user runs `proxlet monitor --once` in a terminal
- **THEN** the same table is printed once and the command exits 0 without taking over the screen

### Requirement: Server not running

When the server is not running, `proxlet monitor` SHALL print an error saying
proxlet is not running, with a hint to start it, and exit with status 1.

#### Scenario: Not running

- **GIVEN** proxlet is not running
- **WHEN** the user runs `proxlet monitor`
- **THEN** stderr says `proxlet is not running` with the hint `proxlet start`, and the exit status is 1
