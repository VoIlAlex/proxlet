# Design

## Context

See proposal.md for the motivation. The current state that shapes the approach:

- The daemon runs as the invoking user with `CAP_NET_BIND_SERVICE`, not as root
  (core design, decision 6). It can bind port 80 but cannot write `/etc/nginx`,
  reload a root-owned nginx, or edit systemd units.
- `sudo proxlet start` and `sudo proxlet stop` run as root, once, and already
  install and remove the proxlet unit through `ServiceManager`.
- The daemon binds one HTTP listener at startup and exits on `EADDRINUSE`. Rule
  snapshots are swapped atomically and are independent of listeners.
- `port::holder` identifies a port's holder through `/proc` or `lsof`. As a
  non-root user it cannot see sockets that belong to root, which includes nginx's
  master process.
- On Debian and Ubuntu, `/var/log/nginx/error.log` is `www-data:adm 0640`. On
  macOS with Homebrew, nginx's config and logs are owned by the user.
- `proxlet-front` exists as an empty crate reserved for this change.

## Goals / Non-Goals

**Goals:**

- Hand port 80 back and forth between proxlet and nginx without dropping
  in-flight requests.
- Keep the request path unchanged: attach only adds a second listener.
- Make every nginx change reversible from a ledger, even after a crash.
- Keep nginx-specific code out of the engine, the CLI and `proxlet-service`.

**Non-Goals:**

- Changing nginx config from the daemon. Only root CLI commands write it.
- Understanding all of nginx's config language. proxlet reads only `include`,
  `http`, `server`, `listen`, `server_name` and `error_log`.
- Making the handoff seamless for an nginx started by hand. It is best effort,
  within nginx's retry window.

## Decisions

### 1. Two listeners: an internal one that is always on, and a public one that comes and goes

The daemon always listens on `127.0.0.1:<attach.port>` (default `47080`). This is
where nginx forwards. The public listener on `listen.address:listen.http_port` is
owned by a port-ownership task, which moves between four states:

| State        | Public listener | Meaning                                            |
| ------------ | --------------- | -------------------------------------------------- |
| `Standalone` | bound           | proxlet owns port 80                               |
| `Attached`   | closed          | nginx owns port 80 and forwards to the internal one |
| `Waiting`    | closed          | another process owns it; proxlet logs and retries  |
| `Released`   | closed          | proxlet let go for nginx and has not seen it bind yet |

In every state except `Standalone`, the task tries to bind every 2 seconds. A
successful bind means `Standalone`. On `EADDRINUSE` it classifies the holder (see
decision 6) and moves to `Attached` or `Waiting`. It logs the "already in use"
error once per holder change, not on every retry.

To release, the task stops accepting and drops the listening socket. Connections
already accepted keep running on their own sockets, so in-flight requests finish.
nginx sets `SO_REUSEADDR`, so it can bind straight away.

From `Released`, the task reclaims as soon as the port is free and no nginx
process is running, and after 10 seconds at the latest.

*Alternative:* `SO_REUSEPORT`, with proxlet and nginx sharing port 80. Rejected:
both sockets need the option and the same user, nginx does not set it by default,
and the kernel would split traffic between the two.

### 2. Privilege split: root CLI writes, daemon only reads and hands over

- **Root commands** (`sudo proxlet start` and `stop`) write nginx config, the
  hook and the ledger, and reload nginx.
- **The daemon** reads nginx config, which is world-readable, to classify holders
  and conflicts. It opens and closes its own listener on request.
- **`proxlet run`** in the foreground sets up the attach itself only when the
  nginx config directory is writable by the user, as with Homebrew on macOS.
  Otherwise `status` says the attach is not set up, as the spec requires.

This keeps the core decision that proxlet does not run as root.

### 3. `FrontServer` trait in `proxlet-front`

```rust
pub trait FrontServer {
    fn name(&self) -> &'static str;                    // "nginx"
    fn detect(&self) -> Option<Installation>;          // binary, conf path, version
    fn is_running(&self) -> bool;                      // master process present
    fn service_unit(&self) -> Option<&'static str>;    // "nginx.service"
    fn read(&self, inst: &Installation) -> Result<FrontConfig, FrontError>;
    fn plan_attach(&self, cfg: &FrontConfig, internal: SocketAddr, take_default: bool) -> Plan;
    fn check(&self, inst: &Installation) -> Result<(), FrontError>;   // nginx -t
    fn reload(&self, inst: &Installation) -> Result<(), FrontError>;  // nginx -s reload
    fn error_log(&self, cfg: &FrontConfig) -> Option<PathBuf>;
    fn is_bind_failure(&self, line: &str, port: u16) -> bool;
}
```

`FrontConfig` holds the server blocks for each port: their `listen` directives,
including the `default_server` flag and file and line, and their `server_name`
entries, which can be exact names, wildcards or regexes. It answers
`claims(host, port) -> Option<Location>` and `default_server(port)`.

A `Plan` is a list of file writes and one-line edits. Applying a plan is generic
(decision 5), so a future Apache or Caddy integration only implements the trait.
The trait takes a `CommandRunner`, as `ServiceManager` does, so it can be tested
without nginx.

### 4. Hooking `nginx.service` through `ServiceManager`, not from `proxlet-front`

`ServiceManager` gains two methods: `hook(unit, before_start, after_stop)` and
`unhook(unit)`. For systemd, `hook` writes
`/etc/systemd/system/nginx.service.d/proxlet.conf` and runs `daemon-reload`:

```ini
[Service]
ExecStartPre=-/usr/local/bin/proxlet handoff release
ExecStopPost=-/usr/local/bin/proxlet handoff reclaim
```

The `-` prefix makes systemd ignore any failure, including a missing binary, so
nginx can always start.

`proxlet handoff release|reclaim` are hidden subcommands. Each sends one control
request, waits up to 3 seconds for the daemon to confirm, and always exits 0.
`release` returns only once the public socket is closed. That ordering is what
makes the service path deterministic.

The front server supplies the unit name, and the service manager supplies the
mechanism, so neither crate depends on the other.

### 5. Applying nginx changes: ledger, check, reload, restore

The ledger is `front.toml` in the state directory, chowned to the service user.
For every file proxlet touches, it records the path, the original bytes or
"absent", and a hash of what proxlet wrote. For the `default_server` takeover, it
also records the file, the line and the original `listen` text.

The apply order is:

1. Write the ledger.
2. Write each file atomically, using a temp file and a rename.
3. Run `nginx -t`.
4. Reload nginx if it is running.

On any failure, proxlet restores the original bytes from the ledger, runs
`nginx -t` again, and reports nginx's own error text. Detach replays the ledger in
reverse.

If the user has edited a file since proxlet wrote it, so the hash no longer
matches, proxlet re-finds the `listen` directive by its text. If it can't find
it, proxlet leaves the file alone and warns, giving the file and line.

The written block, placed in the first `include` directory inside the `http`
block (`conf.d/*.conf`, `sites-enabled/*` or `servers/*`):

```nginx
# Managed by proxlet. Removed by `sudo proxlet stop`.
map $http_upgrade $proxlet_connection { default upgrade; '' close; }
server {
    listen 80 default_server;          # default_server only when taken over or free
    listen [::]:80 default_server;
    server_name _;
    location / {
        proxy_pass http://127.0.0.1:47080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $proxlet_connection;
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 1h;
    }
}
```

If the user declines the takeover, proxlet writes nothing, because a catch-all
without `default_server` would never be chosen.

*Alternative:* `nginx -T` to read the config. Rejected: it needs root on most
hosts, and `add` and `ls` run as the user.

### 6. Recognising nginx as the holder

There are three signals, tried in order:

1. `port::holder` names an `nginx` process.
2. The holder is unknown, an nginx master process is running (process names are
   visible to every user), and nginx config listens on that port.
3. Otherwise the holder is "another process", shown with its name and pid when
   known.

When `attach.enabled` is `off`, step 1 and step 2 are skipped.

### 7. Detecting nginx started by hand: follow its error log

While `Standalone`, the daemon follows nginx's error log (from `error_log` in the
config, or the compiled-in default from `nginx -V`), polling every 100 ms from the
end of the file. A line matching `bind() to <addr>:<port> failed (98: Address
already in use)` (errno 48 on macOS) triggers a release. nginx retries 5 times,
500 ms apart, so a release within about 300 ms lands before its second attempt.

When the log is not readable by the service user, `start` adds
`SupplementaryGroups=<the log file's group>` to the proxlet unit, which is `adm` on
Debian and Ubuntu. If the log is still unreadable, as when nginx is installed
after proxlet, the daemon falls back to process watching: an nginx master that
appears while proxlet holds the port triggers a release. Decision 1's
reclaim-when-no-nginx rule bounds the cost of a false trigger, such as `nginx -t`
run by a package manager, to about 2 seconds.

### 8. Forwarded headers on the internal listener

Requests that arrive on the internal listener come from nginx on loopback.
proxlet trusts their `X-Forwarded-For` and `X-Forwarded-Proto`, and passes them
on unchanged instead of appending `127.0.0.1`. That way the upstream sees the
real client address. On the public listener, behaviour is unchanged.

### 9. Conflicts are checked by the CLI, not the daemon

`add` and `ls` read nginx config directly, which takes milliseconds for typical
configs, so a conflict prompt works whether or not the daemon is running. A
host's conflict is resolved like nginx does it:

1. exact name;
2. leading wildcard;
3. trailing wildcard;
4. regex, matched with the `regex` crate. A pattern it can't compile is skipped,
   with a debug log.

`--yes` is added to `add` and `start`. Prompts appear only when stdin and stderr
are terminals.

### 10. Status

`StatusInfo` gains `mode` values `attached (nginx)` and `waiting for port <n>
(held by <name>)`, plus `attach_ready: bool`, which is true when the ledger shows
an installed catch-all. When attached but not ready, the CLI prints `nginx is not
forwarding to proxlet yet` with the `sudo proxlet start` hint.

## Risks / Trade-offs

- **[A manual nginx start races the error-log watch]** → The release is triggered
  within about 300 ms, well inside nginx's 2-second retry window. The systemd hook
  is the reliable path, and docs recommend `systemctl` over a bare `nginx`.
- **[A false trigger from the process watch, such as `nginx -t`]** → proxlet
  reclaims within about 2 seconds once no nginx is running. This fallback is used
  only when the log is unreadable.
- **[The user edits nginx files proxlet changed]** → Detach checks hashes and
  re-finds the directive by text. If it can't, it warns and leaves the file as it
  is, rather than overwriting the user's work.
- **[proxlet is uninstalled without `stop`]** → The hook's `-` prefix keeps nginx
  starting. The catch-all would forward to a dead port and return 502, so docs tell
  users to run `sudo proxlet stop` before uninstalling.
- **[Port 47080 is taken]** → The internal bind error names the holder and
  suggests `proxlet config attach.port <n>`. It is a startup error like any other.
- **[Unusual nginx layouts with no included directory in `http`]** → `start`
  fails with the nginx config path and asks the user to add an `include` line.
  Nothing is written.

## Migration Plan

- This is a behaviour change only, with no data migration. A taken port no longer
  fails `run` or `start`. The CI step "a failed start is explained" switches to an
  invalid config file, and a new step asserts waiting on a taken port.
- **Rollback:** `sudo proxlet stop` undoes every nginx change from the ledger. If
  the binary is downgraded first, the stale hook is harmless because of the `-`
  prefix, and removing `nginx.service.d/proxlet.conf` and the catch-all file by
  hand restores nginx.
