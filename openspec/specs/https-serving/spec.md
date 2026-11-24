# https-serving Specification

## Purpose
Serves proxlet's rules over HTTPS with the certificates from `certificates`,
both when proxlet owns the HTTPS port and when nginx terminates TLS in front of
it, and moves plain HTTP clients to HTTPS.

## Requirements

### Requirement: HTTPS listener

When proxlet holds `listen.https_port`, it SHALL accept TLS connections there,
choose the certificate by the client's SNI name, and serve the same rules as on
plain HTTP. It SHALL offer HTTP/2 and HTTP/1.1 through ALPN, and SHALL set
`X-Forwarded-Proto: https` on proxied requests. A connection whose name has no
certificate, or that sends no name, SHALL fail the TLS handshake.

#### Scenario: Request over HTTPS

- **GIVEN** a certificate for `example.com` and a rule `example.com/` → port 3000
- **WHEN** a client requests `https://example.com/x`
- **THEN** the TLS handshake uses the `example.com` certificate and the upstream on port 3000 receives `/x` with `X-Forwarded-Proto: https`

#### Scenario: HTTP/2

- **WHEN** a client that supports HTTP/2 connects to `https://example.com/`
- **THEN** the connection uses HTTP/2

#### Scenario: No certificate for the name

- **WHEN** a client connects to the HTTPS port with SNI `other.example.com` and there is no certificate for it
- **THEN** the TLS handshake fails and no certificate is sent

#### Scenario: WebSocket over HTTPS

- **WHEN** a client opens a `wss://` WebSocket on a route proxied to a WebSocket server
- **THEN** messages pass in both directions

### Requirement: Certificate changes apply live

A new, renewed or removed certificate SHALL take effect for TLS handshakes that
start after the change, without a restart and without closing open
connections.

#### Scenario: Renewed while connected

- **GIVEN** a client holds an open HTTPS connection to `example.com`
- **WHEN** the certificate for `example.com` is renewed
- **THEN** the open connection keeps working and the next new connection receives the renewed certificate

### Requirement: Redirect to HTTPS

With `tls.redirect` `on` (the default), a plain HTTP request for a host that has
a certificate SHALL receive a `308` redirect to the same host, path and query
over HTTPS, with the port included when `listen.https_port` is not 443. With
`tls.redirect` `off`, plain HTTP SHALL be served as before.

#### Scenario: Redirected

- **GIVEN** a certificate for `example.com` and HTTPS is being served
- **WHEN** a client requests `http://example.com/a?b=1`
- **THEN** the response is `308` with `Location: https://example.com/a?b=1`

#### Scenario: Redirect off

- **GIVEN** `proxlet config tls.redirect off`
- **WHEN** a client requests `http://example.com/`
- **THEN** the request is served over plain HTTP

### Requirement: When not to redirect

proxlet SHALL NOT redirect ACME challenge requests, requests for hosts without a
certificate, requests that arrive from the front server with
`X-Forwarded-Proto: https`, or any request while HTTPS is not being served
(the HTTPS port is waiting or handed over and no front server forwards it).

#### Scenario: Challenge request

- **GIVEN** a certificate for `example.com` is being renewed
- **WHEN** Let's Encrypt requests `http://example.com/.well-known/acme-challenge/<token>`
- **THEN** proxlet answers the challenge instead of redirecting

#### Scenario: Host without a certificate

- **WHEN** a client requests `http://other.example.com/` and there is no certificate for it
- **THEN** the request is served over plain HTTP as before

#### Scenario: HTTPS not available

- **GIVEN** a certificate for `example.com` and another process holds port 443
- **WHEN** a client requests `http://example.com/`
- **THEN** the request is served over plain HTTP, not redirected

### Requirement: HTTPS behind nginx

When proxlet is attached behind nginx, nginx SHALL terminate TLS on port 443 for
every name it does not serve itself, using proxlet's certificate for the
requested name, and forward the request to proxlet's internal port with
`X-Forwarded-Proto: https`. nginx's own HTTPS sites SHALL keep working.

#### Scenario: Unclaimed name over HTTPS through nginx

- **GIVEN** proxlet is attached behind nginx, nginx serves only `site.example.com`, and proxlet has a certificate for `app.example.com` and a rule for it
- **WHEN** a client requests `https://app.example.com/`
- **THEN** the handshake uses proxlet's certificate for `app.example.com` and proxlet serves the request

#### Scenario: nginx's own HTTPS site

- **GIVEN** nginx serves `site.example.com` on port 443 with its own certificate
- **WHEN** a client requests `https://site.example.com/`
- **THEN** nginx serves it with its own certificate as before

#### Scenario: New certificate behind nginx

- **GIVEN** proxlet is attached behind nginx
- **WHEN** the user runs `proxlet cert app.example.com`
- **THEN** `https://app.example.com/` works through nginx without reloading nginx or running `sudo proxlet start` again
