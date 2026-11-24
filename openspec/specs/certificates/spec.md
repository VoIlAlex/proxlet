# certificates Specification

## Purpose
Lets the user get TLS certificates for their domains from Let's Encrypt (or
another ACME server) with one command, and keeps them valid without further
attention.

## Requirements

### Requirement: Issuing a certificate

`proxlet cert <domain>…` SHALL obtain one certificate per domain from the ACME
server named by `cert.server`, using the HTTP-01 challenge answered by the
running server. It SHALL print progress for each domain and, on success, the
certificate's expiry date. It SHALL exit zero only when every domain got a
certificate. A domain that already has a valid certificate SHALL be left alone
with a message pointing to `proxlet cert renew`.

#### Scenario: Certificate issued

- **GIVEN** proxlet is running and `example.com` resolves to this machine
- **WHEN** the user runs `proxlet cert example.com`
- **THEN** the output ends with `✓ Certificate for example.com, valid until <date>`, the command exits zero, and `proxlet cert ls` lists it

#### Scenario: Several domains

- **WHEN** the user runs `proxlet cert a.example.com b.example.com` and only `b.example.com` passes its checks
- **THEN** `b.example.com` gets a certificate, the output explains why `a.example.com` did not, and the command exits non-zero

#### Scenario: Already has one

- **GIVEN** `example.com` has a certificate valid for 80 more days
- **WHEN** the user runs `proxlet cert example.com`
- **THEN** nothing is requested, the output says it is valid until `<date>` and suggests `proxlet cert renew example.com`, and the command exits zero

#### Scenario: Server not running

- **WHEN** proxlet is not running and the user runs `proxlet cert example.com`
- **THEN** the command exits non-zero, says the server must be running to answer Let's Encrypt's challenge, and suggests `proxlet start` or `proxlet run`

### Requirement: Checks before issuance

Before contacting the ACME server for a domain, proxlet SHALL check that the name
is a public DNS name (not an IP address, `localhost`, or under `.test`,
`.local`, `.localhost`, `.internal` or `.example`), that it resolves, and that
nginx does not serve it on port 80. A failed check SHALL be named with what to
fix, and the ACME server SHALL NOT be contacted for that domain.

#### Scenario: Local name

- **WHEN** the user runs `proxlet cert myapp.test`
- **THEN** the command exits non-zero and says Let's Encrypt cannot issue for `.test` names because they are not public, without contacting it

#### Scenario: Name does not resolve

- **WHEN** the user runs `proxlet cert nothing-here.example.org` and the name has no DNS record
- **THEN** the command exits non-zero and says the name does not resolve, suggesting an `A` or `AAAA` record pointing to this machine

#### Scenario: Name served by nginx

- **GIVEN** `site.example.com` resolves to this machine but nginx serves it on port 80
- **WHEN** the user runs `proxlet cert site.example.com`
- **THEN** the command exits non-zero, says requests for `site.example.com` on port 80 do not reach proxlet, and names nginx's file and line that serves it

### Requirement: Reachability probe

After the name checks pass, proxlet SHALL request a probe path for the domain on
port 80 and SHALL stop, without contacting the ACME server, when the answer
comes from a server other than this proxlet. When the probe cannot be sent from
this machine at all, proxlet SHALL warn and continue, because the ACME server
may still reach it.

#### Scenario: Name answered by another server

- **GIVEN** `app.example.com` resolves to a different machine running a web server
- **WHEN** the user runs `proxlet cert app.example.com`
- **THEN** the command exits non-zero and says `app.example.com` reaches a server other than this proxlet, naming the address it resolves to

#### Scenario: Probe cannot be sent

- **GIVEN** `app.example.com` resolves to this machine's public address, which this machine cannot connect to itself
- **WHEN** the user runs `proxlet cert app.example.com`
- **THEN** the output warns that reachability could not be checked from here, and issuance continues

### Requirement: Terms of service

The first time proxlet registers with an ACME server, it SHALL show the server's
terms-of-service URL and ask the user to accept them. `--yes` SHALL accept them
without a prompt. With no terminal and no `--yes`, the command SHALL exit
non-zero and suggest `--yes`. proxlet SHALL NOT ask for or send an email address.

#### Scenario: First issuance

- **WHEN** the user runs `proxlet cert example.com` for the first time and answers `y`
- **THEN** an ACME account is created without an email address and issuance continues

#### Scenario: No terminal

- **WHEN** `proxlet cert example.com` runs for the first time with stdin not a terminal and no `--yes`
- **THEN** the command exits non-zero, prints the terms URL, and suggests `--yes`

### Requirement: ACME server

The `cert.server` setting SHALL select the ACME server: `production` (Let's
Encrypt), `staging` (Let's Encrypt staging) or an `https://` ACME directory
URL. proxlet SHALL keep a separate ACME account for each server. A domain has
at most one certificate, and `proxlet cert ls` SHALL note which server issued
it unless it is `production`.

#### Scenario: Staging

- **GIVEN** `proxlet config cert.server staging`
- **WHEN** the user runs `proxlet cert example.com`
- **THEN** the certificate comes from Let's Encrypt's staging environment and `proxlet cert ls` marks it `staging`

### Requirement: Answering the challenge

The running server SHALL answer `GET /.well-known/acme-challenge/<token>` for a
pending challenge with the key authorization, on every listener and for every
host, before rule matching. Requests under that path with an unknown token SHALL
be matched against the rules as usual.

#### Scenario: Challenge answered behind nginx

- **GIVEN** proxlet is attached behind nginx and an issuance for `example.com` is in progress
- **WHEN** Let's Encrypt requests `http://example.com/.well-known/acme-challenge/<token>`
- **THEN** nginx forwards it to proxlet and proxlet answers with the key authorization

#### Scenario: Unknown token

- **GIVEN** a files rule for `/` serving a directory that contains `.well-known/acme-challenge/abc`
- **WHEN** a client requests `/.well-known/acme-challenge/abc` and no challenge with that token is pending
- **THEN** the file is served

### Requirement: Storing certificates

Certificates, their private keys and the ACME account key SHALL be stored in
proxlet's state directory. Private keys SHALL NOT be readable by other users,
except by nginx's worker group when proxlet is attached behind nginx. A new or
renewed certificate SHALL be written atomically, so a failure never leaves a
domain with a mismatched certificate and key.

#### Scenario: Key permissions standalone

- **WHEN** a certificate is issued while proxlet is not attached
- **THEN** its private key file is readable only by the user proxlet runs as

### Requirement: Listing certificates

`proxlet cert ls` SHALL print one line per certificate with the domain, the
expiry date, the days left, and a note (`staging`, `renewal failing: <reason>`,
or empty). In a terminal it SHALL be an aligned table; otherwise each line SHALL
be the four fields separated by tabs. With no certificates it SHALL say so and
suggest `proxlet cert <domain>`.

#### Scenario: List

- **GIVEN** a certificate for `example.com` expiring in 60 days
- **WHEN** the user runs `proxlet cert ls | cat`
- **THEN** the output is `example.com\t<date>\t60\t` followed by a newline

#### Scenario: None

- **WHEN** there are no certificates and the user runs `proxlet cert ls`
- **THEN** the output says there are no certificates yet and suggests `proxlet cert <domain>`, and the command exits zero

### Requirement: Removing a certificate

`proxlet cert rm <domain>` SHALL delete the domain's certificate and key, and
the running server SHALL stop using it for new connections. An unknown domain
SHALL exit non-zero and list the domains that have certificates.

#### Scenario: Remove

- **WHEN** the user runs `proxlet cert rm example.com`
- **THEN** `proxlet cert ls` no longer lists it, new HTTPS connections for `example.com` fail the TLS handshake, and plain HTTP for it is no longer redirected

### Requirement: Renewal

The running server SHALL renew each certificate when a third or less of its
lifetime is left (30 days for a 90-day certificate), check at least every 12
hours, and retry a failed renewal with increasing
delays of at most 6 hours. The new certificate SHALL take effect without a
restart. `proxlet cert renew [<domain>…]` SHALL renew the given certificates, or
all of them, immediately, regardless of expiry.

#### Scenario: Due for renewal

- **GIVEN** a certificate with 29 days left
- **WHEN** the server's renewal check runs
- **THEN** a new certificate is obtained, `proxlet cert ls` shows the new expiry, and HTTPS connections opened afterwards use it

#### Scenario: Renewal fails

- **GIVEN** a certificate due for renewal and the ACME server unreachable
- **WHEN** the renewal check runs
- **THEN** the old certificate keeps being served, the error is logged, `proxlet cert ls` notes `renewal failing: <reason>`, and the server retries later

#### Scenario: Renew now

- **WHEN** the user runs `proxlet cert renew example.com`
- **THEN** a new certificate is obtained even though the old one has more than 30 days left, and the output shows the new expiry

### Requirement: Certificate completion

Shell completion SHALL offer the domains of existing host rules after `proxlet
cert`, and the domains that have certificates after `proxlet cert rm` and
`proxlet cert renew`.

#### Scenario: Complete a domain to remove

- **GIVEN** certificates for `a.example.com` and `b.example.com`
- **WHEN** the user presses TAB after `proxlet cert rm `
- **THEN** `a.example.com` and `b.example.com` are offered
