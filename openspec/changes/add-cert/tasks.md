# Tasks

## 1. Settings (proxlet-core)

- [x] 1.1 Add `listen.https_port` (`443`, on restart), `tls.redirect` (`on`, live) and `cert.server` (`production` / `staging` / `https://` URL, live); verify with unit tests for defaults and parsing, and CLI tests for "HTTPS port needs a restart" and "Invalid ACME server"
- [x] 1.2 Validate settings against each other when one is written (HTTPS port differs from `listen.http_port` and `attach.port`), changing nothing on failure; verify with a CLI test for "Same port twice", plus the reverse (changing `listen.http_port` to the HTTPS port)
- [x] 1.3 Document the three settings in the settings table in `docs/usage.md`

## 2. Certificate store (new crate proxlet-cert)

- [x] 2.1 Create `proxlet-cert` with workspace lints and docs, depending on `proxlet-core`, `rcgen` (`ring`, `pem`) and `x509-parser`; add the public-name check from "Checks before issuance" (IP, `localhost`, reserved suffixes, label syntax); verify with unit tests for each rejected kind and for valid names
- [x] 2.2 Add the store (decision 2): one `<domain>.pem` holding the chain and key plus `<domain>.json`, with `load_all`, `get`, `put` (validate key against leaf, names and dates, then atomic rename; mode `0600`), `remove`, and renewal-state updates; verify with tests for a round trip, a mismatched key rejected while the old file stays, permissions, and a crash mid-write (temp file left, store still valid)
- [x] 2.3 Add a fake CA for tests that signs leafs with any validity period using `rcgen`, exported behind a `test-support` feature; verify that the store accepts its output and reports the chosen expiry

## 3. ACME client (proxlet-cert)

- [x] 3.1 Add the `Ca` trait (decision 1) and the order flow: account per server (`<state>/acme/<server-id>/account.json`, P-256, no email), terms check, order, HTTP-01 challenge handed to a `ChallengeSink`, poll, finalize with a fresh P-256 key, store; verify with tests over a fake `Ca` for success, terms needed, a failed challenge (error names the domain and the CA's detail) and a timeout
- [ ] 3.2 Implement `Ca` with `instant-acme` (`ring`); map `production` and `staging` to Let's Encrypt's directory URLs; allow extra trusted roots through `PROXLET_ACME_ROOTS` (a PEM file, for Pebble in tests); verify with a test that runs against Pebble when `PEBBLE_DIRECTORY` is set (skipped otherwise; CI sets it in group 8)
- [x] 3.3 Add the renewal planner (decision 9): due when a third or less of the lifetime is left, backoff of 1h, 2h, 4h, then 6h, with an injectable clock; verify with unit tests for a 90-day certificate (due at 30 days), a 45-day one, and the backoff sequence

## 4. Challenges, redirect and TLS in the engine (proxlet-server)

- [x] 4.1 Add the challenge map and serve `GET /.well-known/acme-challenge/<token>` before rules on every origin; fall through to the rules for unknown tokens; verify with server tests for "Challenge answered" (both listeners) and "Unknown token" (the file is served)
- [x] 4.2 Add `TlsView` and the redirect (decision 4): `308` to `https://host[:port]/path?query`, using the effective scheme per origin; verify with server tests for "Redirected", a non-443 port, "Host without a certificate", "HTTPS not available", "Redirect off", the challenge exemption, and `X-Forwarded-Proto: https` from the front origin
- [x] 4.3 Add `Origin::Tls`, the SNI resolver over an `ArcSwap` map, and TLS accept with the per-connection handshake, a 10 s timeout and ALPN `h2`/`http/1.1` (decision 5); verify with server tests that the handshake picks the right certificate, an unknown name or no SNI fails, HTTP/2 is negotiated, `wss://` works, `X-Forwarded-Proto: https` reaches the upstream, and swapping the map leaves an open connection working while the next handshake gets the new certificate

## 5. Daemon: HTTPS port, issuance and renewal (proxlet)

- [x] 5.1 Run a second port owner for `listen.https_port` with a TLS binder; fan `Release`/`Reclaim` out to both owners and match bind failures for both ports in the watcher; verify with daemon tests for "HTTPS port taken" (HTTP unaffected) and handoff release/reclaim freeing and retaking both ports
- [x] 5.2 Load the store at startup into the resolver and `TlsView`; compute `https_served` from the HTTPS owner's mode and the ledger; rebuild on mode changes and on `tls.redirect` changes; verify with daemon tests that the redirect appears only while HTTPS is served and that `tls.redirect off` applies live
- [x] 5.3 Add the control requests `Probe`, `Issue { domain, accept_terms }`, `Renew { domains }` and `CertsChanged`, with the order mutex and a 120 s client timeout for `Issue`; verify with daemon tests that use the fake CA through a test hook: issue, then the resolver serves the new certificate over HTTPS
- [x] 5.4 Add the renewal task (startup delay, 12 h interval, planner from 3.3), keeping the old certificate on failure and recording the error; verify with daemon tests on short intervals for "Due for renewal" and "Renewal fails"
- [x] 5.5 Extend `StatusInfo` with the HTTPS mode, the address, the certificate count and expiring or failing certificates (`serde(default)` for older servers); verify with tests for the "HTTPS line" and "Certificate expiring" output

## 6. `proxlet cert` commands (proxlet)

- [x] 6.1 Add the `cert` command shape (decision 10): `cert <domain>… [--yes]`, `cert ls`, `cert rm <domain>…`, `cert renew [<domain>…]`, replacing the "not yet" stub; verify with CLI parse tests, including that `ls`, `rm` and `renew` are never taken as domains
- [x] 6.2 Implement issuing: the name, DNS and nginx checks, the probe (stop on another server's answer, warn on a connection error), the terms prompt (or `--yes`, or exit non-zero without a terminal), one domain at a time, and the exit status; verify with CLI tests for "Certificate issued", "Several domains", "Already has one", "Server not running", "Local name", "Name does not resolve", "Name served by nginx", "Name answered by another server", "Probe cannot be sent", "First issuance" and "No terminal", using the fake CA hook and a fake resolver
- [x] 6.3 Implement `cert ls` (table / tab-separated, notes for `staging` and `renewal failing`), `cert rm` (unknown domain lists the known ones; notifies a running daemon) and `cert renew`; verify with CLI tests for "List", "None", "Remove", "Renew now" and "Staging"
- [x] 6.4 Add completion: host-rule domains after `cert`, stored domains after `cert rm` and `cert renew`; verify with completion tests for "Complete a domain to remove"
- [x] 6.5 Document HTTPS and certificates in `docs/usage.md`: issuing, the checks and what they mean, staging, renewal, the redirect, and rate limits

## 7. HTTPS behind nginx (proxlet-front, proxlet)

- [ ] 7.1 Render the port 443 block with the SNI `map` (decision 7) when nginx is 1.15.9 or newer and built with the SSL module; skip it with a warning otherwise; verify with render tests (marker, `default_server`, IPv6, `map` rejecting `../x`) and version-gate tests for 1.14 and 1.28
- [ ] 7.2 Make `plan_attach`, the takeover question and the ledger handle a list of ports (old ledgers read as port 80 only); verify with tests for "Default on port 443 too", a declined 443 takeover, and an old ledger still detaching cleanly
- [ ] 7.3 Create the publish directory as root during `start` (service user, nginx worker group, `2750`) and record it in the ledger; have the daemon copy certificates there (`0640`, atomic) on startup and on every change, delete stale copies, and remove the directory on `stop`; verify with service tests using the fake nginx, and a daemon test that a removed certificate's published copy disappears
- [ ] 7.4 Report in `status` when an attach predates HTTPS forwarding ("HTTPS is not forwarded to proxlet yet", suggesting `sudo proxlet start`); verify with a status test using an old-format ledger
- [ ] 7.5 Update "Behind nginx" in `docs/usage.md` (the 443 block, the publish directory, nginx version needs) and the "Adding a front server" section of `docs/extending.md`

## 8. End to end, CI and release notes

- [ ] 8.1 Add a CI job `acme` that runs Pebble and `pebble-challtestsrv` on the Ubuntu runner, sets the test domains in `/etc/hosts`, and runs standalone: `proxlet cert` with `--yes`, an HTTPS request with Pebble's root, the redirect, `wss://`, `cert renew`, `cert ls`, `cert rm` (handshake then fails), and the Pebble-gated crate test from 3.2; verify that the job passes
- [ ] 8.2 Extend the CI `nginx` job with HTTPS: `sudo proxlet start --yes` installs the 443 block, a certificate issued while attached is served through nginx without a reload, nginx's own HTTPS site still works, and `stop` still restores `/etc/nginx` exactly (`diff -r`); verify that the job passes
- [ ] 8.3 Update `README.md` (status and roadmap) and `CHANGELOG.md` (including the BREAKING redirect default), and write `verification.md` mapping every scenario to its test; verify with `openspec validate add-cert --strict`

## Workflow follow-up

- Push the branch, open a PR, and merge once CI passes.
- Archive the change and sync its specs.
