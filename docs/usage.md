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

Booleans accept `on`/`off` and `true`/`false`. Durations are a number with `ms`,
`s` or `m`, for example `500ms`, `30s` or `2m`. `cert.server` is `production`
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

proxlet and nginx can share port 80. nginx keeps serving its own sites, and
every request for a host nginx does not serve goes to proxlet.

```sh
sudo proxlet start          # attaches if nginx is installed
proxlet status              # mode    attached (nginx)
```

### Who holds port 80

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
block includes, such as `/etc/nginx/conf.d/proxlet.conf`. It is a server block
marked `default_server` that forwards to proxlet with the original `Host`,
`X-Forwarded-For` and `X-Forwarded-Proto`, WebSocket upgrades, and no
buffering.

If another nginx server is already the default for port 80 (Debian and Ubuntu
ship one), proxlet names it and asks:

```text
? nginx sends requests for hosts it does not serve to its default server at
  /etc/nginx/sites-enabled/default:22. Make proxlet the default instead? [y/N]
```

- **Yes** (or `sudo proxlet start --yes`): proxlet removes `default_server` from
  that line and puts it on its own block.
- **No**, or no terminal: proxlet changes nothing in nginx, and requests for
  hosts nginx does not serve keep going to nginx's default server.

Every change is checked with `nginx -t`, written atomically, and followed by a
reload. If any step fails, every file is put back and nginx keeps running its
previous config.

`sudo proxlet stop` undoes all of it: it removes proxlet's file and the systemd
drop-in, restores the `default_server` flag, and reloads nginx. If you edited
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

HTTPS (port 443) is not attached yet; it arrives with certificates.

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
