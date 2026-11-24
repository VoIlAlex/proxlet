# Extending proxlet

proxlet has three extension points. Each is one trait in its own crate, and a
new integration is a new module implementing that trait. Nothing in the engine,
the CLI or the other integrations changes.

| Extension point    | Crate                    | Trait            | Today   |
| ------------------ | ------------------------ | ---------------- | ------- |
| Service managers   | `crates/proxlet-service` | `ServiceManager` | systemd |
| Front servers      | `crates/proxlet-front`   | `FrontServer`    | nginx   |
| AI tools           | `crates/proxlet-ai`      | *(AI change)*    | —       |

## Adding a service manager

`proxlet start` and `proxlet stop` only talk to `ServiceManager`. To add, say,
launchd or OpenRC:

1. **Create the module.** Add `crates/proxlet-service/src/<name>.rs` with a struct
   that takes a `CommandRunner`, the way `Systemd<R>` does. Injecting the runner
   is what lets you test without the real tool.

2. **Implement the trait.**

   | Method        | Must do |
   | ------------- | ------- |
   | `name`        | Short name, shown to users and passed to the daemon as `PROXLET_SERVICE` |
   | `is_present`  | Cheap check that this manager runs the host, with no side effects |
   | `install`     | Write the service definition for the `ServiceSpec`; return `false` if nothing changed |
   | `start`       | Enable at boot and start now |
   | `stop`        | Stop now and disable at boot |
   | `state`       | Map the manager's state to `ServiceState`; report a crash-and-retry as `Restarting` |
   | `recent_logs` | The service's latest log lines, newest last |
   | `hook`        | Run commands before another service starts and after it stops; a failing command must never block that service |
   | `unhook`      | Remove proxlet's hook from another service |

   Honour every field of `ServiceSpec`:
   - run as `run_as`, or as root when it is `None`;
   - pass every `env` pair;
   - when `bind_privileged_ports` is set, allow binding ports below 1024 without
     full root;
   - join every group in `supplementary_groups`.

3. **Register it.** Add it to `system_managers()` in
   `crates/proxlet-service/src/lib.rs`, in the order it should be detected, and
   add its name to `SUPPORTED`.

4. **Test it.** Use a recording `CommandRunner` (see the tests in `systemd.rs`)
   and assert on:
   - the service definition you generate;
   - the exact commands for `start`, `stop` and `state`;
   - the hook definition, and that `unhook` leaves other overrides alone;
   - how each state the tool reports maps to `ServiceState`.

5. **Document it.** Add the manager and its file locations to
   [`docs/usage.md`](usage.md#as-a-system-service).

The CLI already handles readiness waiting, explaining a failed start from
`recent_logs`, root checks and `sudo` users, so a manager needs nothing beyond
the trait.

### Skeleton

```rust
use proxlet_service::{
    CommandRunner, ServiceError, ServiceManager, ServiceSpec, ServiceState, UnitHook,
};

pub struct Launchd<R> {
    runner: R,
}

impl<R: CommandRunner> ServiceManager for Launchd<R> {
    fn name(&self) -> &'static str {
        "launchd"
    }
    fn is_present(&self) -> bool {
        cfg!(target_os = "macos")
    }
    fn install(&self, spec: &ServiceSpec) -> Result<bool, ServiceError> {
        // Write /Library/LaunchDaemons/dev.proxlet.plist from `spec`.
        todo!()
    }
    fn start(&self) -> Result<(), ServiceError> {
        // launchctl bootstrap system /Library/LaunchDaemons/dev.proxlet.plist
        todo!()
    }
    fn stop(&self) -> Result<(), ServiceError> {
        // launchctl bootout system/dev.proxlet
        todo!()
    }
    fn state(&self) -> Result<ServiceState, ServiceError> {
        // launchctl print system/dev.proxlet
        todo!()
    }
    fn recent_logs(&self, lines: usize) -> Vec<String> {
        Vec::new()
    }
    fn hook(&self, unit: &str, hook: &UnitHook) -> Result<bool, ServiceError> {
        // launchd has no drop-ins; say so, and attach still works through the
        // front server's error log.
        Ok(false)
    }
    fn unhook(&self, unit: &str) -> Result<bool, ServiceError> {
        Ok(false)
    }
}
```

## Adding a front server

Attach mode talks to front servers only through `FrontServer` in
`crates/proxlet-front`. Applying a change, checking it, reloading, rolling back
and undoing it on `stop` are generic (`proxlet_front::apply`), so a new front
server only describes itself. To add, say, Apache:

1. **Create the module.** Add `crates/proxlet-front/src/<name>/mod.rs` with a
   struct that takes a `CommandRunner`, as `Nginx<R>` does.

2. **Implement the trait.**

   | Method           | Must do |
   | ---------------- | ------- |
   | `name`           | Short name, shown in `status` (`attached (<name>)`) |
   | `detect`         | Find the binary, version, main config file and default error log |
   | `is_running`     | Whether the main process runs, without needing root |
   | `service_unit`   | The unit to hook, e.g. `apache2.service`, or `None` |
   | `read`           | Parse the config into `FrontConfig`: server blocks, their ports, names and default flags, and directories included where server blocks may go |
   | `https_support`  | Whether this installation can terminate TLS with proxlet's certificates (version, TLS module); the reason when it can't |
   | `plan_attach`    | The file to write (catch-alls on the HTTP port and, when given an `HttpsForward`, on the HTTPS port, forwarding to proxlet's internal address) and any line edits to move default-server flags; a port that keeps its own default gets no block, and a plan with no block is empty |
   | `check`          | Validate the config, e.g. `apachectl configtest` |
   | `reload`         | Reload a running server gracefully |
   | `error_log`      | Where the server logs bind failures |
   | `is_bind_failure`| Recognise its "address already in use" line for a port |
   | `holds`          | Recognise the server as a port's holder, including when the holder can't be seen |

   Mark every file you write with a first line proxlet recognises (nginx uses
   `# Managed by proxlet.`), so `read` can tell proxlet's block from the user's.
   The HTTPS block must read `<cert_dir>/<name>.pem` (chain and key in one
   file) for the requested name at handshake time, so new certificates need no
   reload, and must only accept plain DNS names when building that path.
   `read` should also fill `worker_user` and `worker_group`, so `start` can let
   the workers read the certificate directory.

3. **Register it.** Add it to the list of front servers the CLI and daemon try.

4. **Test it.** Build realistic config trees in a temporary directory (see
   `crates/proxlet-front/tests/nginx_layouts.rs`) and assert on what `read`
   finds and what `plan_attach` writes. Use a recording `CommandRunner` for
   `detect`, `check` and `reload`.
