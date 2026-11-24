# Using proxlet

## Rules

A rule sends requests that match a **route** to a **target**.

```sh
proxlet add /api 8000              # any host, /api and below  -> localhost:8000
proxlet add example.com/api 8002   # only example.com/api      -> localhost:8002
proxlet add example.com 3000       # everything on example.com -> localhost:3000
proxlet add /files .               # serve the current directory under /files
```

Adding a route that already has a rule replaces its target and tells you what
it was before. `proxlet rm <route>` removes a rule, and `proxlet ls` lists them all.

Each change is checked before anything is written. A rejected change leaves
the configuration exactly as it was. If proxlet is running, the change takes
effect on the next request, without a restart and without dropping requests
that are already in flight.

### Routes

| You write         | Matches                                              |
| ----------------- | ---------------------------------------------------- |
| `/api`            | `/api` and `/api/...` on any host                    |
| `example.com/api` | `/api` and `/api/...` on `example.com` only          |
| `example.com`     | everything on `example.com` (same as `example.com/`) |
| `/`               | everything on any host                               |

Hosts are case-insensitive and never include a port or scheme. Trailing slashes
are ignored, so `/api/` and `/api` are the same route.

### Targets

proxlet reads a target in this order:

1. **A bare number** is a port on this machine: `8000` means `http://127.0.0.1:8000`.
2. **`host:port`** or an **`http://` / `https://` URL** is an upstream server:
   `10.0.0.5:9000`, `https://api.internal`.
3. **Anything else** is a directory: `.`, `./public`, `/srv/site`. It is stored as
   an absolute path. To serve a directory whose name is a number, write `./8000`.

If nothing is listening on a port target yet, `add` still succeeds and warns
you. Requests get `502` until your server starts.

### How a request is matched

1. Rules for the request's exact host are tried first, then rules with no host.
2. Within each group, the rule with the longest path wins.
3. Paths match on segment boundaries: `/api` matches `/api/users` but not `/apix`.

With rules for `/`, `/api` and `example.com/admin`:

| Request                   | Served by           |
| ------------------------- | ------------------- |
| `example.com/admin/users` | `example.com/admin` |
| `example.com/api/x`       | `/api`              |
| `other.org/api`           | `/api`              |
| `other.org/about`         | `/`                 |

A request that matches no rule gets a `404` that names the host and path.

### Proxying

Requests are forwarded with their path and query unchanged, including the rule's
prefix: with `proxlet add /api 8000`, a request for `/api/users?page=2` reaches
the upstream as `/api/users?page=2`.

The upstream sees the browser's original `Host` header, plus `X-Forwarded-For`,
`X-Forwarded-Proto` and `X-Forwarded-Host`. Request and response bodies are
streamed. WebSocket and other protocol upgrades pass straight through.

When an upstream cannot be reached proxlet answers `502`. When it accepts the
connection but sends no response within `proxy.timeout`, proxlet answers `504`.
Both responses name the route and the upstream.

### Serving files

A files rule serves its directory with the route's prefix removed: with
`proxlet add /files ~/site`, `/files/img/logo.png` is `~/site/img/logo.png`.

- Content types come from the file extension. Conditional (`If-None-Match`,
  `If-Modified-Since`) and range requests are supported.
- A directory request returns its `index.html` if there is one. Otherwise it
  returns a listing, or `404` when `static.listing` is off. A directory requested
  without a trailing slash is redirected to the slash form so relative links work.
- Nothing outside the directory is ever served, whether through `..`, encoded
  separators or symbolic links that point outside it.
- Files whose name starts with a dot (`.env`, `.git/`) are hidden unless
  `static.dotfiles` is on.

## Settings

```sh
proxlet config                        # list every setting
proxlet config static.listing         # print one value
proxlet config static.listing off     # change it
```

| Setting                 | Default      | Applies     | What it does                                                   |
| ----------------------- | ------------ | ----------- | -------------------------------------------------------------- |
| `listen.address`        | `0.0.0.0`    | on restart  | Address the HTTP and HTTPS listeners bind to                   |
| `listen.http_port`      | `80`         | on restart  | Port the HTTP listener binds to                                |
| `listen.https_port`     | `443`        | on restart  | Port the HTTPS listener binds to                               |
| `static.listing`        | `on`         | immediately | List directories that have no `index.html`                     |
| `static.dotfiles`       | `off`        | immediately | Serve and list files whose name starts with a dot              |
| `proxy.connect_timeout` | `5s`         | immediately | How long to wait for an upstream to accept a connection        |
| `proxy.timeout`         | `60s`        | immediately | How long to wait for an upstream to start responding           |
| `attach.enabled`        | `on`         | immediately | Attach behind nginx when it holds or wants the port            |
| `attach.port`           | `47080`      | on restart  | Loopback port nginx forwards to when proxlet is attached       |
| `tls.redirect`          | `on`         | immediately | Redirect plain HTTP to HTTPS for hosts that have a certificate |
| `cert.server`           | `production` | immediately | Where certificates come from: `production`, `staging` or a URL |
| `log.access`            | `on`         | immediately | Record every request in the [log files](#logs)                 |
| `log.max_size`          | `100MB`      | immediately | Delete the oldest log files beyond this total size             |
| `log.max_age`           | `7d`         | immediately | Delete log files older than this                               |

Booleans accept `on`/`off` and `true`/`false`. Durations are a number with `ms`,
`s`, `m`, `h` or `d`, for example `500ms`, `30s`, `2m` or `7d`. Sizes are a
number with `KB`, `MB` or `GB`, in powers of 1024, for example `100MB` or `1GB`;
`log.max_size` is at least `10MB`, the size of one log file. `cert.server` is `production`
(Let's Encrypt), `staging` (Let's Encrypt's test environment, whose
certificates browsers do not trust) or the `https://` directory URL of any ACME
server. The HTTP, HTTPS and attach ports must all differ.

## Running

```sh
proxlet run       # in the foreground; Ctrl-C stops it gracefully
proxlet status    # running? where? how many rules?
```

If the HTTP port is free, proxlet listens on it. If nginx holds it, proxlet
attaches behind nginx (see [Behind nginx](#behind-nginx)). If anything else
holds it, proxlet keeps running, logs which process is in the way, and takes
the port as soon as it is free; `proxlet status` shows
`waiting for port 80 (held by caddy)` meanwhile.

On shutdown proxlet stops accepting connections and gives in-flight requests up
to 10 seconds to finish.

`proxlet status` exits `0` when proxlet is running and `3` when it is not, so it
works in scripts.

### As a system service

```sh
sudo proxlet start    # install, enable at boot, start, and wait until it serves
sudo proxlet stop     # stop and disable at boot; rules and settings are kept
```

`proxlet start` needs root once, to install the service. The service runs as
**you** (the user who ran `sudo`), not as root:

- It can serve your own folders.
- Its rules and settings are your usual files.
- `add`, `rm`, `ls` and `config` never need `sudo`.

On systemd the unit gets only the capability to bind ports below 1024.

If the server fails to start, for example because its configuration file has a
mistake, `proxlet start` shows the server's own error rather than a generic
service failure, and stops the service so it doesn't retry in a loop. A port
held by another process is not a failure: `proxlet start` warns and the server
waits for it.

Supported service managers: **systemd** (Linux). On macOS, or on Linux without
systemd, use `proxlet run`. `proxlet stop` also stops a foreground `proxlet run`.

Installed files on systemd:

| What  | Where                                 |
| ----- | ------------------------------------- |
| Unit  | `/etc/systemd/system/proxlet.service` |
| Log   | `journalctl -u proxlet.service`       |
| nginx hook (when attached) | `/etc/systemd/system/nginx.service.d/proxlet.conf` |

The unit is regenerated on every `proxlet start`, so after upgrading or moving
the binary just run `sudo proxlet start` again.

## HTTPS and certificates

proxlet gets certificates from Let's Encrypt and serves HTTPS on port 443 with
them, choosing the certificate by the name the client asks for.

```sh
proxlet add app.example.com 3000
proxlet cert app.example.com     # ✓ Certificate for app.example.com, valid until 2027-02-22
curl https://app.example.com/
```

For this to work, `app.example.com` must be a public name whose DNS points to
this machine, and port 80 must reach proxlet from the internet: Let's Encrypt
checks that you control the name by fetching a token from
`http://app.example.com/.well-known/acme-challenge/`, and the running proxlet
answers it. So proxlet must be running (`proxlet start` or `proxlet run`).

### Checks before asking Let's Encrypt

Let's Encrypt limits failed attempts (five per name per hour), so `proxlet
cert` first checks what it can and tells you what to fix:

| Check                    | When it fails                                                                 |
| ------------------------ | ----------------------------------------------------------------------------- |
| The name is public       | `.test`, `.local`, `.localhost`, `.internal`, `.example`, IP addresses, `*.` |
| The name resolves        | add an `A` or `AAAA` record pointing to this machine                          |
| nginx does not serve it  | nginx would answer the challenge instead of proxlet; the file and line shown |
| A probe reaches proxlet  | the name answers from another server; its address is shown                    |

If the probe cannot be sent from this machine at all (common behind NAT,
which often cannot reach its own public address), `proxlet cert` warns and
asks Let's Encrypt anyway.

The first time, you are asked to accept Let's Encrypt's terms of service;
`--yes` accepts them without a prompt. proxlet never asks for or sends an email
address. Let's Encrypt no longer sends expiry emails; `proxlet status` and
`proxlet cert ls` show expiry instead.

### Managing certificates

```sh
proxlet cert ls                    # domain, expiry, days left, notes
proxlet cert renew app.example.com # renew now (all certificates when none given)
proxlet cert rm app.example.com    # delete it; HTTPS for that name stops
```

The server renews each certificate when a third of its lifetime is left (30
days for a 90-day certificate), checking twice a day. A failed renewal keeps
the old certificate, is logged, retried after 1, 2, 4 and then every 6 hours,
and shown in `cert ls` and `status`. New and renewed certificates take effect
without a restart and without closing open connections.

To experiment without touching Let's Encrypt's limits, use its staging
environment (browsers do not trust its certificates; `cert ls` marks them):

```sh
proxlet config cert.server staging
```

`cert.server` also takes the `https://` directory URL of any other ACME server
that does not need an account binding.

### Plain HTTP and the redirect

Once a host has a certificate, plain HTTP requests for it get a `308` redirect
to HTTPS, keeping the path and query. ACME challenges are never redirected, and
nothing is redirected while HTTPS is not actually being served (for example
while another program holds port 443). Turn it off with
`proxlet config tls.redirect off`.

Port 443 follows the same rules as port 80: proxlet listens on it when it is
free, serves behind nginx when nginx holds it, and waits when anything else
does. `proxlet status` shows it on the `https` line. Without root on Linux,
binding 443 fails; set `listen.https_port` to a high port, as with
`listen.http_port`.

## Behind nginx

proxlet and nginx can share ports 80 and 443. nginx keeps serving its own
sites, and every request for a host nginx does not serve goes to proxlet, over
plain HTTP and over HTTPS.

```sh
sudo proxlet start          # attaches if nginx is installed
proxlet status              # mode    attached (nginx)
                            # https   attached (nginx)
```

### Who holds port 80

Port 443 follows exactly the same rules; nginx starting or stopping hands over
or takes back both ports together.

| Situation                      | proxlet                                                        |
| ------------------------------ | -------------------------------------------------------------- |
| Port 80 is free                | listens on it (`standalone`)                                   |
| nginx holds it                 | serves behind nginx on `127.0.0.1:47080` (`attached (nginx)`)  |
| nginx starts while proxlet holds it | hands the port over; nginx starts normally                 |
| nginx stops                    | takes port 80 back within a few seconds                        |
| anything else holds it         | logs the holder and waits (`waiting for port 80`)              |

proxlet hears about nginx starting in two ways:

- **nginx as a service.** `sudo proxlet start` adds a small systemd drop-in to
  `nginx.service`, so `systemctl start nginx` asks proxlet to release the port
  first. The hook can never stop nginx from starting, even if proxlet is gone.
- **nginx started by hand** (`sudo nginx`, or on macOS). proxlet follows nginx's
  error log and releases the port when nginx fails to bind it; nginx retries
  for about two seconds, which is plenty. If proxlet cannot read the log, it
  watches for an nginx process to appear instead. If nginx then does not take
  the port within 10 seconds, proxlet takes it back.

`systemctl` is the reliable path; prefer it over a bare `nginx`.

### What proxlet changes in nginx

`sudo proxlet start` writes one file into the first directory nginx's `http`
block includes, such as `/etc/nginx/conf.d/proxlet.conf`. It holds two server
blocks marked `default_server`, one for port 80 and one for port 443, that
forward to proxlet with the original `Host`, `X-Forwarded-For` and
`X-Forwarded-Proto`, WebSocket upgrades, and no buffering.

The port 443 block terminates TLS with proxlet's certificate for the name the
client asks for, which nginx reads when the connection is made. So a new
`proxlet cert` works through nginx right away, with no reload, and nginx's own
HTTPS sites keep their own certificates. A name proxlet has no certificate for
fails the handshake (nginx logs it). This needs nginx 1.15.9 or newer built
with TLS; with an older nginx, only the port 80 block is written and `start`
says so.

nginx's workers cannot read proxlet's state directory in your home, so
`sudo proxlet start` creates `/var/lib/proxlet/nginx-certs` (yours, readable by
nginx's worker group, mode `2750`) and the server keeps a copy of each
certificate there, removed with the certificate. `sudo proxlet stop` deletes
the directory.

If another nginx server is already the default for port 80 or 443 (Debian and
Ubuntu ship one for port 80), proxlet names each of them and asks once:

```text
? nginx sends requests for hosts it does not serve to its default server at
  /etc/nginx/sites-enabled/default:22. Make proxlet the default instead? [y/N]
```

- **Yes** (or `sudo proxlet start --yes`): proxlet removes `default_server` from
  that line and puts it on its own block.
- **No**, or no terminal: those ports keep nginx's default server, and
  requests on them for hosts nginx does not serve keep going there. A port
  with no default server of its own still gets proxlet's block.

Every change is checked with `nginx -t`, written atomically, and followed by a
reload. If any step fails, every file is put back and nginx keeps running its
previous config.

`sudo proxlet stop` undoes all of it: it removes proxlet's file, the systemd
drop-in and the certificate directory, restores the `default_server` flags, and
reloads nginx. If you edited
one of those lines in the meantime, proxlet leaves it alone and tells you.

What proxlet changed is recorded in `front.json` in its state directory, so
`stop` can undo it even after a crash. **Run `sudo proxlet stop` before
uninstalling proxlet.**

Without root, `proxlet run` sets up the attach only when it may write nginx's
config (as with Homebrew on macOS), and undoes it when it exits.

### Hosts nginx already serves

```text
$ proxlet add site.example 3000
! nginx serves site.example (/etc/nginx/sites-enabled/site:3); requests for it will not reach proxlet
? Add the rule anyway? [y/N]
```

Without a terminal, `add` refuses unless you pass `--yes`. Such rules are
marked in `proxlet ls`:

```text
ROUTE         KIND   TARGET                 NOTE
site.example  proxy  http://127.0.0.1:3000  served by nginx (/etc/nginx/sites-enabled/site:3)
```

In plain output (`proxlet ls | cat`) the note is a fourth tab-separated field,
empty for other rules.

### Turning it off

`proxlet config attach.enabled off` makes proxlet treat nginx like any other
process holding the port: it never reads, writes or hooks nginx's config.

If proxlet attached before it could forward HTTPS, `proxlet status` says
`HTTPS is not forwarded to proxlet yet`; run `sudo proxlet start` again to add
the port 443 block.

## Logs

The running server records every request it answers and its own messages,
and `proxlet logs` reads them back, filtered and followed.

```sh
proxlet logs                       # the last 20 records, then follow new ones
proxlet logs /api                  # only requests answered by the /api rule
proxlet logs example.com --all     # every stored request for a domain, then exit
proxlet logs 8000 --all --json     # requests proxied to port 8000, as JSON
proxlet logs --all | grep " 502 "  # plain lines when piped
```

In a terminal each record is one aligned, coloured line; piped, the same text
without colour:

```text
01:06:12.040  200  GET    example.com/api/users          12ms    512 B  → 127.0.0.1:8000
01:06:13.501  502  GET    example.com/api                 1ms     96 B  → 127.0.0.1:8000  connection refused
01:06:20.002  WARN        port 443 is already in use by caddy (pid 812); waiting for it to be free
```

Times are local; the date is added for records from another day. Ctrl-C stops
following. `proxlet logs` reads the files directly, so it works while proxlet
is stopped, and a crashed server's last records are still there.

### Selectors

- **A route**, starting with `/` or written `host/path` (`/api`,
  `example.com/api`, `example.com/` for a host's root rule): requests answered
  by exactly that rule.
- **A number** (`8000`): requests proxied to an upstream on that port.
- **Anything else** (`example.com`): requests for that host, whatever rule
  answered them.

Server events are shown only without a selector. Selectors tab-complete from
your rules.

### What is recorded

Each request is recorded once its response has been sent, or the client went
away: the time it arrived, the client address, method, host, path, status,
bytes sent, duration, the rule and its upstream or directory, and for a proxy
error the reason. Long responses and WebSockets are recorded when they end;
for a WebSocket that is the `101` handshake, without the tunnelled bytes.
Behind nginx, the client is the address nginx saw, not nginx itself.

Query strings, headers and bodies are never recorded. Client addresses are
personal data in some places: the files are readable only by the user proxlet
runs as, and kept for 7 days by default. `proxlet config log.access off` stops
recording requests; server events are still recorded.

The server's own messages of level info and above (listening, port handovers,
attaching, configuration changes, certificates) are recorded too, and still
printed to the terminal or the journal as before.

Recording never holds up a request. When records arrive faster than they can
be written, or the disk refuses them, they are dropped, and a warning record
later says how many.

### The files

Records are JSON, one object per line, in `~/.local/state/proxlet/logs/`
(`/var/lib/proxlet/logs/` when proxlet runs as root without `sudo`). `proxlet.log` is the current
file; at 10 MB it is renamed to `proxlet-<unix-millis>.log` and a new one is
started. The oldest files are deleted so all of them together stay within
`log.max_size` (`100MB`), and files older than `log.max_age` (`7d`) are
deleted. Both apply immediately.

`--json` prints the stored lines unchanged. The fields are stable:

| Field    | Kind    | Meaning                                                         |
| -------- | ------- | --------------------------------------------------------------- |
| `t`      | both    | UTC time, RFC 3339 with milliseconds                            |
| `kind`   | both    | `request` or `event`                                            |
| `client` | request | Client address                                                  |
| `method` | request | HTTP method                                                     |
| `host`   | request | Lowercase host without the port; absent if the request had none |
| `path`   | request | Path without the query                                          |
| `status` | request | Response status                                                 |
| `bytes`  | request | Body bytes sent                                                 |
| `ms`     | request | Duration in milliseconds, to a tenth                            |
| `rule`   | request | The rule that answered; absent when none did                    |
| `target` | request | Upstream `host:port`, directory, `acme-challenge` or `redirect` |
| `error`  | request | Why the upstream failed (502, 504)                              |
| `level`  | event   | `info`, `warn` or `error`                                       |
| `msg`    | event   | The message                                                     |

```json
{"t":"2026-11-24T01:06:13.501Z","kind":"request","client":"203.0.113.7","method":"GET","host":"example.com","path":"/api","status":502,"bytes":96,"ms":0.8,"rule":"/api","target":"127.0.0.1:8000","error":"connection refused"}
{"t":"2026-11-24T01:06:20.002Z","kind":"event","level":"warn","msg":"port 443 is already in use by caddy (pid 812); waiting for it to be free"}
```

## Monitor

`proxlet monitor` shows a live table of your rules with their traffic, one row
per rule and a last `(no rule)` row for everything no rule answered (no match,
redirects to HTTPS, certificate challenges). It refreshes every second.

```sh
proxlet monitor                    # the live table; q to quit
proxlet monitor --once             # print the table once and exit
proxlet monitor | grep /api        # plain table when piped
```

```text
RULE       TARGET          REQ/S  TOTAL  4XX  5XX  UP-ERR   P50   P95   P99   OUT/S  ACTIVE  WS
/api       127.0.0.1:8000    4.2    512    3    1       1  12ms  40ms  88ms  20KB/s       1   0
(no rule)  -                 0.0      2    2    0       0     -     -     -    0B/s       0   0
```

| Column | What it counts |
| --- | --- |
| `REQ/S` | Requests per second over the last 10 seconds |
| `TOTAL` | Requests since the server started |
| `4XX`, `5XX` | Responses with those statuses since the server started |
| `UP-ERR` | 502/504 answers proxlet gave because the upstream could not be reached; an upstream's own 5xx counts under `5XX` only |
| `P50`, `P95`, `P99` | Latency over the last 60 seconds, from the request arriving to the response headers being ready; `-` without requests |
| `OUT/S` | Response body bytes sent per second over the last 10 seconds |
| `ACTIVE` | Requests whose response has not ended yet |
| `WS` | Open WebSocket connections |

Keys: `s` changes the sort (route, requests per second, errors, p95), the
arrow keys and `PgUp`/`PgDn` scroll, and `q`, `Esc` or Ctrl-C quit. If the
server stops while the table is open, it says so and picks up again when the
server is back.

The numbers live in the running server's memory: counting never slows a
request down, works with `log.access off`, and starts again from zero when the
server restarts. A rule keeps its numbers when its target changes; removing
it and adding it again starts it from zero.

## The configuration file

Rules and settings live in one TOML file, which you can also edit by hand.
CLI edits keep your comments and ordering.

```toml
[listen]
http_port = 8080

[static]
listing = false

[[rule]]
route = "/api"
proxy = "http://127.0.0.1:8000"

[[rule]]
route = "example.com/files"
files = "/home/me/site"
```

If the file has a mistake, every command reports the file, line and problem,
and refuses to overwrite it.

## Where proxlet keeps its files

| What             | Default location                 |
| ---------------- | -------------------------------- |
| Configuration    | `~/.config/proxlet/proxlet.toml` |
| State and socket | `~/.local/state/proxlet/`        |
| Certificates     | `~/.local/state/proxlet/certs/`  |
| Logs             | `~/.local/state/proxlet/logs/`   |

- `XDG_CONFIG_HOME` and `XDG_STATE_HOME` are respected.
- `sudo proxlet ...` uses the invoking user's files, not root's.
- Running as root without `sudo` uses `/etc/proxlet` and `/var/lib/proxlet`.
- `PROXLET_HOME=/some/dir` puts everything under one directory, which is handy
  for trying things out.

## Shell completion

Commands, setting names, setting values and your existing rules all
tab-complete. Enable completion once for your shell:

```sh
echo 'source <(COMPLETE=zsh proxlet)' >> ~/.zshrc                    # zsh
echo 'source <(COMPLETE=bash proxlet)' >> ~/.bashrc                  # bash
echo 'COMPLETE=fish proxlet | source' >> ~/.config/fish/config.fish  # fish
```
