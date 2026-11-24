# Proposal

## Why

proxlet only speaks plain HTTP today, so anything exposed on a real domain is
unencrypted, and browsers increasingly refuse or warn on it. Developers want
`proxlet cert example.com` to give them working HTTPS from Let's Encrypt, both
when proxlet owns the ports itself and when it sits behind nginx. Port 443 was
explicitly deferred from `add-nginx-attach` to this change.

## What Changes

- **`proxlet cert <domain>…`** gets a Let's Encrypt certificate for each domain
  through the HTTP-01 challenge, which the running server answers on port 80.
  Before contacting Let's Encrypt, proxlet checks that the domain is a public
  name, resolves, and reaches this proxlet over HTTP, and explains what to fix
  when it does not. The first issuance asks the user to accept Let's Encrypt's
  terms (`--yes` accepts them).
- **`proxlet cert ls`** lists certificates with their domains, expiry and
  renewal state. **`proxlet cert rm <domain>`** deletes one. **`proxlet cert
  renew [<domain>]`** renews now.
- **Automatic renewal.** The server renews every certificate about 30 days
  before it expires, retries failures with backoff, and logs the outcome.
  `proxlet status` warns about certificates that are close to expiring.
- **HTTPS standalone.** proxlet listens on `listen.https_port` (443) and
  terminates TLS itself, picking the certificate by SNI. It offers HTTP/2 and
  HTTP/1.1. New and renewed certificates take effect without a restart and
  without dropping connections. Port 443 follows the same ownership rules as
  port 80: listen when free, attach behind nginx, wait when anything else holds
  it.
- **HTTPS behind nginx.** `sudo proxlet start` also installs a catch-all server
  block on port 443 in nginx that terminates TLS with proxlet's certificate for
  the requested name and forwards to proxlet's internal port. nginx keeps its
  own HTTPS sites. The `default_server` takeover question and the `--yes` flag
  cover port 443 too. Handoff, reclaim and detach work for both ports.
- **Redirect to HTTPS.** Plain HTTP requests for a host that has a certificate
  are redirected to HTTPS (`308`), except ACME challenge requests, and only
  while HTTPS is actually being served. **BREAKING:** after `proxlet cert`,
  plain HTTP for that host redirects by default; `tls.redirect off` keeps the
  old behaviour.
- **New settings:** `listen.https_port` (`443`, restart), `tls.redirect`
  (`on`, live), `cert.server` (`production`; also `staging` or an ACME
  directory URL; live).
- **Status** shows HTTPS alongside HTTP: the mode of port 443, the listening
  address, and the number of certificates.

### Out of scope

- The DNS-01 challenge, wildcard certificates, and machines not reachable from
  the internet: a later change that adds a DNS provider trait.
- A local development CA for `.test` and `localhost` names (mkcert-style): a
  later change.
- Issuing certificates automatically when a host rule is added: not planned;
  issuance stays explicit.
- Bringing your own certificate files: a later change if asked for.
- Certificate authorities that need External Account Binding (ZeroSSL,
  Google): a later change. Any ACME server without EAB works through
  `cert.server`.
- HTTP/3, OCSP stapling, mutual TLS: not planned.
- Expiry alerts by email: Let's Encrypt no longer sends them, and proxlet will
  not ask for an email address.
- Access and error logs (`proxlet logs`): the *logs* change.

## Capabilities

### New Capabilities

- `certificates`: obtaining, storing, listing, removing and renewing TLS
  certificates through ACME (Let's Encrypt, HTTP-01), including the checks
  before issuance and the challenge responder.
- `https-serving`: serving HTTPS with those certificates, standalone and behind
  nginx, and redirecting plain HTTP to HTTPS.

### Modified Capabilities

- `daemon-lifecycle`: port ownership, `start` and `status` cover the HTTPS port
  as well as the HTTP port.
- `front-attach`: the nginx catch-all, default-server takeover, handoff,
  reclaim and detach cover port 443.
- `settings`: adds `listen.https_port`, `tls.redirect` and `cert.server`.

## Impact

- **Code:** new crate `proxlet-cert` (ACME client, certificate store, renewal
  schedule); `proxlet-server` (TLS listener with an SNI certificate resolver,
  challenge responder and redirect in the engine); `proxlet` (a second port
  owner, `cert` commands, control requests for issuing and renewing, `status`);
  `proxlet-front` (443 block, takeover and bind-failure matching for both
  ports); `proxlet-core` settings.
- **Dependencies:** `instant-acme` (ACME), `rcgen` (keys and CSRs),
  `tokio-rustls` (TLS server), `x509-parser` (expiry).
- **External state:** certificates and the ACME account key in proxlet's state
  directory; requests to the ACME server; when attached, a second server block
  in nginx's proxlet file, which nginx reads certificates for.
- **CI:** a new job runs Pebble (Let's Encrypt's test server) and issues,
  serves, renews and removes a certificate, standalone and behind nginx.
- **Docs:** `docs/usage.md` (HTTPS and certificates), README.
