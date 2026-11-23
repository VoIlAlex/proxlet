# Extending proxlet

proxlet has three extension points. Each is one trait in its own crate, and a
new integration is a new module implementing that trait. Nothing in the engine,
the CLI or the other integrations changes.

| Extension point    | Crate                    | Trait            | Today   |
| ------------------ | ------------------------ | ---------------- | ------- |
| Service managers   | `crates/proxlet-service` | `ServiceManager` | systemd |
| Front servers      | `crates/proxlet-front`   | *(attach change)* | —       |
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

   Honour every field of `ServiceSpec`:
   - run as `run_as`, or as root when it is `None`;
   - pass every `env` pair;
   - when `bind_privileged_ports` is set, allow binding ports below 1024 without
     full root.

3. **Register it.** Add it to `system_managers()` in
   `crates/proxlet-service/src/lib.rs`, in the order it should be detected, and
   add its name to `SUPPORTED`.

4. **Test it.** Use a recording `CommandRunner` (see the tests in `systemd.rs`)
   and assert on:
   - the service definition you generate;
   - the exact commands for `start`, `stop` and `state`;
   - how each state the tool reports maps to `ServiceState`.

5. **Document it.** Add the manager and its file locations to
   [`docs/usage.md`](usage.md#as-a-system-service).

The CLI already handles readiness waiting, explaining a failed start from
`recent_logs`, root checks and `sudo` users, so a manager needs nothing beyond
the trait.

### Skeleton

```rust
use proxlet_service::{
    CommandRunner, ServiceError, ServiceManager, ServiceSpec, ServiceState,
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
}
```
