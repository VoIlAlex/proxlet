# Proposal

## Why

On a server that already runs nginx, proxlet cannot serve anything today: port 80
is taken, so `proxlet start` fails. Developers want proxlet to coexist with the
sites nginx already serves, and to keep working when nginx comes and goes, without
editing nginx config by hand.

## What Changes

- **Port ownership.** proxlet listens on port 80 itself when it is free. When nginx
  holds it, proxlet attaches behind nginx instead. When anything else holds it,
  proxlet logs an error naming the holder and keeps retrying until the port is
  free. **BREAKING:** a taken port no longer makes `proxlet run` and
  `proxlet start` fail; they start, warn, and wait.
- **Handoff to nginx.** When nginx starts while proxlet holds port 80, proxlet
  releases the port so nginx can bind it, and keeps serving behind nginx. It learns
  about nginx starting in two ways: a systemd drop-in for `nginx.service` that calls
  proxlet before nginx starts, and a watch for nginx's "address already in use"
  failures, which covers nginx started by hand and on macOS.
- **Reclaim.** When nginx stops or is removed, proxlet takes port 80 back.
- **Thin attach.** proxlet installs one nginx server block that forwards all
  traffic nginx does not claim to proxlet's internal loopback port. Routing stays
  in proxlet. Config changes are validated with `nginx -t`, written atomically,
  reloaded, and rolled back if any step fails.
- **Default server takeover.** If nginx already marks one of its own servers as
  the default for port 80, proxlet asks for confirmation, then moves the
  `default_server` flag to its own block. `--yes` confirms without a prompt.
- **Detach.** `proxlet stop` removes proxlet's server block and the systemd
  drop-in, restores any `default_server` flag it moved, and reloads nginx.
- **Domain conflicts.** `proxlet add` warns when nginx already serves the rule's
  host, names the nginx file and line, and asks for confirmation. `proxlet ls`
  marks such rules.
- **Status** shows the mode: standalone, attached to nginx, or waiting for the
  port (with the holder).
- **New settings:** `attach.enabled` (`on`) and `attach.port` (`47080`).
- **Extension point:** a `FrontServer` trait in `proxlet-front`, with nginx as the
  first implementation. The service-manager trait gains a way to hook another
  unit, so front servers never depend on systemd directly.

### Out of scope

- Port 443 and TLS, including nginx forwarding HTTPS to proxlet: the *cert* change
  (`add-cert`).
- Attaching individual rules to nginx by command (per-host server blocks): a later
  change, to be discussed separately.
- Front servers other than nginx (Apache, Caddy): later changes using the
  `FrontServer` trait.
- Hooks for service managers other than systemd (for example launchd): the change
  that adds that service manager.
- `logs`, `monitor` and `ai`: their own changes.

## Capabilities

### New Capabilities

- `front-attach`: attaching proxlet behind a front server (nginx first): detecting
  it, handing port 80 over and reclaiming it, installing and removing the
  catch-all server block and hooks, taking over the default server, and warning
  about domains the front server already serves.

### Modified Capabilities

- `daemon-lifecycle`: a taken port no longer stops the server (wait or attach
  instead); `start` sets up the attach and `stop` removes it; `status` reports the
  mode; the "Start fails" scenario now uses an invalid config file.
- `proxy-rules`: `proxlet ls` marks rules whose host the front server already
  serves.
- `settings`: adds `attach.enabled` and `attach.port`.

## Impact

- **Code:** `proxlet-front` (new `FrontServer` trait, nginx config reader and
  writer); `proxlet-service` (unit hooks on `ServiceManager`, systemd drop-ins,
  supplementary group for reading nginx's log); `proxlet` daemon (port-ownership
  loop, internal listener, handoff control requests), `start`, `stop`, `status`,
  `add`, `ls`; `proxlet-core` settings.
- **External state:** writes one file into nginx's config directory, one systemd
  drop-in for `nginx.service`, and may edit one `listen` directive in an existing
  nginx file. All of it is recorded in proxlet's state directory so `stop` can undo
  it.
- **CI:** the `systemd` job gains an nginx scenario; the taken-port step now
  expects waiting instead of failure.
- **Docs:** `docs/usage.md` (attach mode), `docs/extending.md` (adding a front
  server).
