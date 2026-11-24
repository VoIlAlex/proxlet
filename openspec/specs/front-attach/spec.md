# front-attach Specification

## Purpose
Lets proxlet run alongside a front server (nginx first): proxlet hands port 80 to
nginx when nginx wants it, takes it back when nginx goes away, and receives every
request nginx does not claim, without the user editing nginx config by hand.

## Requirements

### Requirement: Catch-all server block

With nginx installed and `attach.enabled` `on`, `sudo proxlet start` SHALL
install nginx server blocks on ports 80 and 443 that forward every request
nginx does not claim to `127.0.0.1:<attach.port>` with the original `Host`,
`X-Forwarded-For`, `X-Forwarded-Proto` and WebSocket upgrades, without
buffering. The 443 block SHALL use proxlet's certificate for the requested
name, read at handshake time. An nginx older than 1.15.9 or without TLS SHALL
get only the port 80 block, with a warning.

#### Scenario: Unclaimed host reaches proxlet

- **GIVEN** nginx serves only `site.example` and proxlet has a rule `/api` → port 8000
- **WHEN** a client requests `http://other.example/api/x` on port 80
- **THEN** the request reaches the upstream on port 8000 with `Host: other.example`

#### Scenario: Claimed host stays with nginx

- **GIVEN** nginx serves `site.example`
- **WHEN** a client requests `http://site.example/` on port 80
- **THEN** nginx serves it as before and proxlet does not see the request

#### Scenario: WebSocket through nginx

- **WHEN** a client opens a WebSocket on a route proxlet proxies, through nginx
- **THEN** messages flow both ways between the client and the upstream

#### Scenario: HTTPS block installed

- **WHEN** the user runs `sudo proxlet start` with nginx 1.28 installed
- **THEN** proxlet's nginx file has a port 443 server block marked `default_server` that forwards to `127.0.0.1:<attach.port>`, and `nginx -t` passes even before any certificate exists

#### Scenario: nginx too old for HTTPS forwarding

- **WHEN** the user runs `sudo proxlet start` with nginx 1.14 installed
- **THEN** only the port 80 block is installed, the output warns that HTTPS is not forwarded to proxlet because nginx 1.15.9 or newer is needed, and the command exits zero

### Requirement: Safe nginx config changes

Every change proxlet makes to nginx config SHALL be checked with `nginx -t`,
written atomically, and followed by a reload when nginx is running. If the check
or the reload fails, proxlet SHALL restore every file it changed, leave nginx
running its previous config, and exit non-zero showing nginx's error.

#### Scenario: nginx rejects the change

- **GIVEN** an existing nginx config error that `nginx -t` reports
- **WHEN** the user runs `sudo proxlet start`
- **THEN** the command exits non-zero, prints nginx's error, and the nginx config directory is byte-identical to before

### Requirement: Default server takeover

If another nginx server is already marked `default_server` for port 80 or port
443, proxlet SHALL name each such file and line and ask once before taking over.
On yes, it SHALL move the flags to its own blocks. On no, or with no terminal
and no `--yes`, it SHALL leave those flags unchanged, warn that unclaimed traffic
on those ports stays with those servers, and still start. Ports with no default
server SHALL get proxlet's block either way.

#### Scenario: Take over with confirmation

- **GIVEN** `/etc/nginx/sites-enabled/default` line 2 has `listen 80 default_server;`
- **WHEN** the user runs `sudo proxlet start` and answers `y`
- **THEN** that line no longer has `default_server`, proxlet's block has it, and unclaimed hosts reach proxlet

#### Scenario: Take over without a prompt

- **WHEN** the user runs `sudo proxlet start --yes` in the same situation
- **THEN** the flag is moved without asking

#### Scenario: Declined

- **WHEN** the user runs `sudo proxlet start` and answers `n`
- **THEN** nginx config is unchanged, the output warns that unclaimed traffic goes to the server at `/etc/nginx/sites-enabled/default:2`, and the command exits zero

#### Scenario: Default on port 443 too

- **GIVEN** `/etc/nginx/sites-enabled/main` line 5 has `listen 443 ssl default_server;` and line 2 has `listen 80 default_server;`
- **WHEN** the user runs `sudo proxlet start`
- **THEN** one question names both lines, and answering `y` moves both flags to proxlet's blocks

### Requirement: Handoff when nginx starts as a service

On a systemd host, `sudo proxlet start` SHALL hook `nginx.service` so that, before
nginx starts, proxlet releases ports 80 and 443 and keeps serving on its
internal port. The hook SHALL never stop nginx from starting, even when proxlet
is not running or not installed.

#### Scenario: nginx starts while proxlet holds port 80

- **GIVEN** proxlet is running standalone on ports 80 and 443
- **WHEN** the user runs `sudo systemctl start nginx`
- **THEN** nginx starts successfully, `proxlet status` shows `mode    attached (nginx)` and `https   attached (nginx)`, and proxlet's rules are still served on ports 80 and 443 through nginx

#### Scenario: proxlet not running

- **GIVEN** proxlet is stopped but its hook is still installed
- **WHEN** the user runs `sudo systemctl start nginx`
- **THEN** nginx starts normally

### Requirement: Handoff when nginx is started directly

While proxlet holds port 80 or port 443, it SHALL watch for nginx failing to bind
either port and release both within nginx's retry window, so that nginx started
by hand or by another launcher succeeds. If nginx has not taken a released port
10 seconds after the release, proxlet SHALL take it back.

#### Scenario: nginx started by hand

- **GIVEN** proxlet is running standalone on ports 80 and 443
- **WHEN** the user runs `sudo nginx`
- **THEN** nginx starts successfully and proxlet's rules are served through nginx on both ports

#### Scenario: nginx fails for another reason

- **GIVEN** proxlet released its ports because nginx tried to bind them
- **WHEN** nginx exits without binding them
- **THEN** proxlet listens on ports 80 and 443 again within 15 seconds of the release

### Requirement: Reclaim when nginx stops

When proxlet is attached and port 80 or port 443 becomes free, proxlet SHALL
listen on it again within 5 seconds and serve its rules directly, over TLS on
the HTTPS port.

#### Scenario: nginx stopped

- **GIVEN** proxlet is attached behind nginx
- **WHEN** the user runs `sudo systemctl stop nginx`
- **THEN** within 5 seconds proxlet serves its rules on ports 80 and 443, and `proxlet status` shows `mode    standalone` and an `https` line with proxlet's address

### Requirement: Detach on stop

`sudo proxlet stop` SHALL remove proxlet's server blocks and its `nginx.service`
hook, restore any `default_server` flag it moved on either port, and reload
nginx, using the same check-and-restore safety as other nginx config changes.

#### Scenario: Stop while attached

- **GIVEN** proxlet is attached and took over the default server
- **WHEN** the user runs `sudo proxlet stop`
- **THEN** the nginx config files are as they were before `proxlet start`, `nginx -t` passes, and unclaimed hosts are served by nginx's original default server

### Requirement: Attach set up after nginx is installed

If nginx is installed while proxlet is already running, `sudo proxlet start` SHALL
set up the attach and report what it did. Until then, `proxlet status` SHALL say
that the attach is not set up and how to fix it.

#### Scenario: nginx installed later

- **GIVEN** proxlet is running and nginx is installed and started afterwards
- **WHEN** the user runs `proxlet status`
- **THEN** the output says `nginx is not forwarding to proxlet yet` and suggests `sudo proxlet start`

#### Scenario: Setting up the attach

- **WHEN** the user then runs `sudo proxlet start`
- **THEN** the catch-all block and hook are installed, the output says proxlet attached to nginx, and the command exits zero

### Requirement: Domains nginx already serves

`proxlet add` SHALL warn when nginx config already serves the rule's host on port
80, naming the nginx file and line, and ask before adding. With no terminal it
SHALL exit non-zero unless `--yes` is given. Declining SHALL change nothing.

#### Scenario: Conflict confirmed

- **GIVEN** `/etc/nginx/sites-enabled/site` line 3 has `server_name site.example;`
- **WHEN** the user runs `proxlet add site.example 3000` and answers `y`
- **THEN** the output warns that nginx serves `site.example` (`/etc/nginx/sites-enabled/site:3`) and requests for it will not reach proxlet, and the rule is added

#### Scenario: Conflict without a terminal

- **WHEN** `proxlet add site.example 3000` runs with stdin not a terminal and no `--yes`
- **THEN** the command exits non-zero, prints the same warning, suggests `--yes`, and adds nothing

#### Scenario: Conflict declined

- **WHEN** the user answers `n`
- **THEN** the command exits non-zero and no rule is added

### Requirement: Attach can be turned off

With `attach.enabled` set to `off`, proxlet SHALL treat nginx like any other
process holding port 80 and SHALL NOT read, write or hook nginx config.

#### Scenario: Attach disabled

- **GIVEN** `proxlet config attach.enabled off` and nginx holds port 80
- **WHEN** the user runs `sudo proxlet start`
- **THEN** no nginx file or hook is created, and `proxlet status` shows `mode    waiting for port 80 (held by nginx)`
