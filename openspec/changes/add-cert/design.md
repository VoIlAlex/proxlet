# Design

## Context

See proposal.md for why. The current state this design builds on:

- **Port ownership** (`crates/proxlet/src/ports.rs`): one `Owner` task holds the
  public HTTP listener and moves between `Standalone`, `Attached`, `Waiting` and
  `Released`. It takes `Release` and `Reclaim` commands from the control socket
  (sent by the nginx systemd hook) and from the log watcher
  (`crates/proxlet/src/watch.rs`). An internal listener on
  `127.0.0.1:<attach.port>` is always on, and nginx forwards to it.
- **Engine** (`crates/proxlet-server`): `accept(listener, engine, conns,
  origin, stop)` serves plain TCP through hyper-util's auto builder (HTTP/1.1
  and HTTP/2 already compiled in). `Origin::Front` trusts `X-Forwarded-Proto`
  from nginx; `Origin::Direct` does not. The rule snapshot lives in an
  `ArcSwap`.
- **nginx attach** (`crates/proxlet-front`): `render()` produces one port 80
  catch-all, `plan_attach` moves `default_server` flags, and a ledger
  (`front.json`) records every change so `stop` can undo it. Only root
  `start`/`stop` write nginx config; the daemon runs as the invoking user and
  only reads.
- **State** lives in `~/.local/state/proxlet` of the user proxlet runs as, even
  under `sudo` (`Locations::resolve`). On Ubuntu that home is usually `0750`,
  so nginx's workers (`www-data`) cannot read anything under it.
- rustls 0.23 with the `ring` provider is already in the tree, through
  `hyper-rustls` for HTTPS upstreams.

## Goals / Non-Goals

**Goals:**

- One TLS stack (rustls, `ring`) for upstream HTTPS, the HTTPS listener and
  ACME.
- Issuance and renewal run in the daemon, the only process that can answer the
  challenge; the CLI reports progress.
- No root needed for issuance, renewal or removal, even when attached behind
  nginx.
- No blocking work on the request path: challenge lookups, the redirect
  decision and certificate selection are lock-free reads.

**Non-Goals:**

- Sharing certificates with nginx's own sites, or importing nginx's existing
  certificates.
- ACME Renewal Information (ARI) and certificate profiles. The one-third rule
  works for today's 90-day certificates and the coming shorter ones.
- Revoking certificates on `cert rm`. Removal only deletes them locally.

## Decisions

### 1. A new crate, `proxlet-cert`

It holds the ACME client wrapper, the certificate store, name checks and the
renewal schedule. It depends on `proxlet-core` but not on `proxlet-server`. The
server only needs an interface that resolves a name to a key and a set of names
that have certificates. That keeps ACME and its dependencies out of the engine
and lets the store be tested without a network.

The ACME client sits behind a small `Ca` trait (`register`, `order`, `finalize`
→ chain PEM). `instant-acme` provides the real one, and a fake CA built with
`rcgen` signs certificates locally with any validity period, for tests.
*Alternative:* `rustls-acme`, which drives ACME from inside the TLS acceptor
with TLS-ALPN-01. Rejected: it hides issuance from the CLI, so the checks and
messages in the spec can't be done, and behind nginx TLS-ALPN-01 can't work
because nginx terminates TLS.

Dependencies: `instant-acme` 0.8 (`ring`), `rcgen` 0.14 (`ring`, `pem`; for the
key and CSR), `x509-parser` 0.18 (expiry and names of the stored leaf), and
`tokio-rustls` 0.26 in `proxlet-server`. All use `ring`, so there is one crypto
provider, installed once at startup.

### 2. Store layout: one PEM file per domain

```
<state>/certs/<domain>.pem    chain + private key, mode 0600
<state>/certs/<domain>.json   {server, not_before, not_after, last_error, next_attempt}
<state>/acme/<server-id>/account.json
```

The chain and key live in one file, so replacing it is a single atomic rename,
and a crash can never pair a new chain with an old key. nginx accepts one file
for both `ssl_certificate` and `ssl_certificate_key`. Before the rename, a new
file is checked: the key matches the leaf, the leaf names the domain, and the
dates parse. Otherwise the old file stays. `<server-id>` is `production`,
`staging`, or a short hash of a custom directory URL. The `.json` holds the
renewal state, so `proxlet cert ls` can read everything from disk without the
daemon. Domain names are validated before they are used as file names, so there
is no path traversal.

*Alternative:* certbot's `live/` symlinks over `archive/` versions. Rejected as
more moving parts for no gain at this scale.

### 3. Issuance runs in the daemon; checks run in the CLI

`proxlet cert d1 d2` handles one domain at a time:

1. **CLI checks:** the name, then DNS through the system resolver, then nginx's
   config (`attach::front_config`, as `add` already does).
2. **CLI probe:** a new control request `Probe` makes the daemon register a
   random token in the challenge map for 30 seconds. The CLI fetches
   `http://<domain>/.well-known/acme-challenge/<token>`, connecting to the
   resolved address. The right body means proxlet answered. A different answer
   means another server did, so the CLI stops. A connection error or timeout
   means hairpin NAT or a firewall, so the CLI warns and goes on.
3. **Issue:** a new control request `Issue { domain, accept_terms }`. The daemon
   loads or creates the account; with no account and `accept_terms` false, it
   replies `TermsNeeded { url }` and the CLI asks, then retries. It then
   places the order, puts the key authorization in the challenge map, tells the
   ACME server it's ready, polls, finalizes with a fresh P-256 key, validates and
   stores the result, swaps the resolver, and replies `Issued { not_after }`.
   Orders are serialized by a mutex shared with renewal.

The control client's timeout for `Issue` is 120 seconds, and the CLI prints a
progress line before sending it. *Alternative:* streaming progress events over
the socket. Deferred: one line per domain is enough, and the control protocol
stays request/response.

### 4. The engine checks challenges and redirects before rules

`Engine` gains two `ArcSwap`s:

- `challenges: HashMap<token, key_authorization>`.
- `tls: TlsView { names: HashSet<String>, https_served: bool, https_port: u16,
  redirect: bool }`.

`handle_from` checks them in this order:

1. A `GET` under `/.well-known/acme-challenge/` with a known token: answer it,
   on any origin and host.
2. A redirect when all of these hold: `redirect`, `https_served`, the host is in
   `names`, and the effective scheme is `http`. The effective scheme is
   `https` for `Origin::Tls`, `X-Forwarded-Proto` for `Origin::Front`, and
   `http` otherwise. The redirect is `308` to `https://host[:port]/path?query`.
3. The rules, as today.

`https_served` is set by the daemon: the HTTPS owner is `Standalone`, or it is
`Attached` and the ledger records an installed port 443 block. So proxlet never
redirects to a port nobody is serving.

### 5. TLS listener: the same `accept`, with the handshake per connection

`accept` takes an optional `TlsAcceptor`. The TLS handshake runs inside the
spawned per-connection task, with a 10-second timeout, never in the accept
loop, so a slow client can't stall accepting. Its `Origin` is `Origin::Tls`,
which sets `X-Forwarded-Proto: https`. ALPN offers `h2` and `http/1.1`.
hyper-util's auto builder already serves both, and WebSocket clients use
HTTP/1.1.

The `ServerConfig` uses a resolver that implements `ResolvesServerCert`
over an `ArcSwap<HashMap<String, Arc<CertifiedKey>>>`, keyed by lowercase name.
No SNI or an unknown name returns `None`, and rustls fails the handshake.
Swapping the map affects only handshakes that start after the swap, which gives
"live, without dropping connections".

### 6. Two port owners, released and reclaimed together

The daemon runs a second `Owner` for `listen.https_port`, with a binder that
opens that port and an `accept` that wraps it in TLS. The `Owner` itself is
unchanged; the port-specific parts are already parameters. The daemon then
fans out each command:

- **`Release`** from the hook or the watcher goes to both owners. The reply
  comes once both listeners are closed.
- **`Reclaim`** goes to both.
- **The watcher** matches bind failures for either port (`is_bind_failure(line,
  port)` is called with each).
- **`Probe::classify`** already takes the port.

`status` shows each owner's `Mode`. The HTTP one is on the `mode` line, as
today, and the HTTPS one on a new `https` line.

*Alternative:* one owner with a list of ports. Rejected: the two ports really
are independent (nginx may hold 80 but not 443, or another program may hold
443), and the state machine is already tested for a single port.

### 7. nginx's 443 block reads proxlet's certificate per handshake

`render` adds a second server block when the HTTPS forward is enabled:

```nginx
map $ssl_server_name $proxlet_cert {
    ~^[A-Za-z0-9.-]+$  $ssl_server_name;
    default            _none;
}
server {
    listen 443 ssl default_server;
    listen [::]:443 ssl default_server;
    server_name _;
    ssl_certificate     <published>/$proxlet_cert.pem;
    ssl_certificate_key <published>/$proxlet_cert.pem;
    location / { …same proxy settings, X-Forwarded-Proto $scheme… }
}
```

Variables in `ssl_certificate` (nginx 1.15.9 and later) load the file at
handshake time, so a new certificate works without a reload. `nginx -t` passes
before any certificate exists. The `map` allows only letters, digits, dots and
hyphens, so a crafted SNI value can't steer nginx to another file. The version
comes from `nginx -V`, which is already parsed. For older versions, or a build
without `--with-http_ssl_module`, there is no port 443 block, and `start`
warns.

The takeover question in `start` collects default servers for both ports.
`plan_attach` takes a list of ports, the ledger records which ports got a
block, and `undo` is unchanged, because it only replays ledger entries.

### 8. Publishing certificates where nginx's workers can read them

nginx's workers load the files as nginx's `user` (Debian `www-data`), which
can't traverse the service user's home. As root, `start` creates a publish
directory, `/var/lib/proxlet/nginx-certs`:

- owned by the service user, with group set to the primary group of nginx's
  `user` directive (default `nobody`);
- mode `2750`, so files created in it inherit that group;
- recorded in the ledger.

The daemon copies each certificate file there (mode `0640`, atomic rename) on
startup and after every issue, renewal or removal, and deletes copies whose
certificate is gone. `stop` removes the directory along with the rest of the
attach.

When `proxlet run` attaches to a user-writable nginx (Homebrew), the publish
directory is `<state>/certs` itself. There, nginx runs as the same user, so no
copy is needed.

*Alternative:* storing certificates outside the home from the start
(`/var/lib/proxlet/certs`). Rejected: it would need root for `proxlet run`, and
it would split the state directory.

### 9. Renewal schedule

There is one renewal task in the daemon. It runs at startup (after a random 0–5
minute delay, so restarts don't hit the CA all together) and then every 12
hours. A certificate is due when `now >= not_after - lifetime / 3`. A failure
writes `last_error`, and `next_attempt = now + min(1h × 2^(n-1), 6h)`, to the
`.json`, and the old certificate stays in use. `proxlet cert renew` sends
`Renew { domains }`, which skips the due check. The renewal task and `Issue`
share the order mutex. The ACME server used is the current `cert.server`, so
switching to staging and running `renew` moves a domain to staging.

The clock and both intervals are parameters, so tests can run with short
intervals and a fake CA (the approach `ports.rs` tests already use).

### 10. CLI shape

`proxlet cert` is a clap command with optional subcommands:

- `proxlet cert <domain>…` issues. `--yes` accepts the terms.
- `proxlet cert ls`, `proxlet cert rm <domain>…` and `proxlet cert renew
  [<domain>…]`.

They use `args_conflicts_with_subcommands`. `ls`, `rm` and `renew` are never
valid public domain names, since they have no dot, so they can't be mistaken
for domains. Completion offers host-rule domains for issuing and stored domains
for `rm` and `renew`, using the existing dynamic completers.

`cert ls` and `cert rm` work without the daemon. `rm` deletes the files, then
sends `CertsChanged` if the daemon is running, so it swaps the resolver and
refreshes the published copies. Without the daemon, `rm` also deletes the
published copy if it can.

### 11. Settings

- `listen.https_port`: `443`, needs a restart (like `listen.http_port`).
- `tls.redirect`: `on`, live; it rebuilds `TlsView`.
- `cert.server`: `production`, live; it is read at each order.

Settings are validated against each other when written: the HTTPS port must
differ from `listen.http_port` and `attach.port`. Changing it is refused, and
nothing is written.

## Risks / Trade-offs

- **[Risk] Hairpin NAT** makes the probe fail on some networks even though
  Let's Encrypt can reach the machine. → Only a clear answer from another
  server stops issuance. Connection errors warn and continue.
- **[Risk] Let's Encrypt rate limits**: 5 failed validations per hostname per
  hour, and 50 certificates per domain per week. → The checks catch most
  failures before any order. The docs point to `cert.server staging` for
  experiments. CI uses Pebble, never Let's Encrypt.
- **[Risk] Taking over the default server on 443** changes what clients with
  no SNI, or an unknown name, see: before, nginx's first TLS site answered
  them; now the handshake fails. → It's part of the takeover question, so the
  user is asked first, and declining leaves port 443's default alone.
- **[Trade-off] Per-handshake certificate loading in nginx** costs a file read
  per handshake, and an unknown name logs an nginx error. → That's fine for a
  development proxy. The `map` sends bad names to `_none`, which fails fast.
- **[Risk] A private key in a second place** (the publish directory). → It's
  only readable by nginx's worker group, removed on `stop`, and refreshed on
  every change, so a removed certificate doesn't linger.
- **[Risk] A redirect loop behind other proxies** that send
  `X-Forwarded-Proto: http` over TLS. → Only nginx's internal-listener origin
  is trusted, and nginx sets the header from `$scheme`.
- **[Trade-off] No email address**, so there's no expiry mail. Let's Encrypt
  stopped sending it anyway. Expiry shows in `status` and `cert ls`, and
  failures are logged.

## Migration Plan

- Existing installs get `listen.https_port 443` and the other new defaults on
  upgrade. Nothing changes until the user runs `proxlet cert`, apart from
  proxlet also trying to hold port 443. If 443 is taken, proxlet waits on it
  and port 80 is unaffected.
- An install already attached behind nginx gets the 443 block on the next
  `sudo proxlet start`. Until then, `status` says HTTPS is not forwarded and
  suggests `sudo proxlet start`. The ledger gains a `ports` field, and old
  ledgers read as port 80 only.
- Rollback: `sudo proxlet stop` removes both blocks, the hook and the publish
  directory. Certificates stay in the state directory.

## Open Questions

- Whether `proxlet cert ls` should also show the issuer's name (for example
  `R11`). This is cosmetic and can be decided during implementation.
