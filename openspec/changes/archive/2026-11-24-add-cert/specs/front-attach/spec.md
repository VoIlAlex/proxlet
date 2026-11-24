## MODIFIED Requirements

### Requirement: Catch-all server block

When nginx is installed and `attach.enabled` is `on`, `sudo proxlet start` SHALL
install nginx server blocks that forward every request nginx does not claim to
proxlet on `127.0.0.1:<attach.port>`: one for port 80 and one for port 443. The
port 443 block SHALL terminate TLS with proxlet's certificate for the requested
name, read when the handshake happens, so new certificates need no nginx reload.
Both blocks SHALL pass the original `Host`, the client address in
`X-Forwarded-For`, the scheme in `X-Forwarded-Proto`, and WebSocket upgrades,
and SHALL NOT buffer responses. When nginx is older than 1.15.9 or lacks TLS
support, proxlet SHALL install only the port 80 block and warn that HTTPS is not
forwarded.

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
