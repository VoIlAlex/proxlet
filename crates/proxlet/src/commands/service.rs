//! `proxlet start` and `proxlet stop`.
//!
//! Everything host-specific (the service manager, whether we are root, who
//! invoked `sudo`, how to probe the daemon) is gathered in [`Host`], so the
//! command logic can be tested against a fake manager.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use proxlet_core::Locations;
use proxlet_service::{RunAs, SUPPORTED, ServiceManager, ServiceSpec, ServiceState};

use crate::control::{self, Client, Request, StatusInfo};
use crate::daemon::SERVICE_ENV;
use crate::output::{self, CmdResult, Failure};

/// The host as `start`/`stop` see it.
pub struct Host {
    /// The detected service manager, if any.
    pub manager: Option<Box<dyn ServiceManager>>,
    /// Whether this process runs as root.
    pub is_root: bool,
    /// The user who ran `sudo`, if any (the service runs as them).
    pub invoking_user: Option<RunAs>,
    /// The proxlet executable to install.
    pub exe: PathBuf,
    /// Asks the daemon for its status; `None` when it is not running.
    pub probe: Box<dyn Fn() -> Option<StatusInfo>>,
    /// Asks a running daemon to shut down; returns whether it accepted.
    pub request_shutdown: Box<dyn Fn() -> bool>,
    /// How long to wait for the daemon to come up or go away.
    pub timeout: Duration,
    /// How often to check while waiting.
    pub poll: Duration,
}

impl Host {
    /// The real host.
    #[must_use]
    pub fn system(locations: &Locations) -> Self {
        let is_root = nix::unistd::geteuid().is_root();
        let socket = locations.socket();
        let socket2 = socket.clone();
        Self {
            manager: proxlet_service::detect(proxlet_service::system_managers()),
            is_root,
            invoking_user: if is_root { sudo_user() } else { None },
            exe: std::env::current_exe()
                .and_then(|p| p.canonicalize())
                .unwrap_or_else(|_| PathBuf::from("proxlet")),
            probe: Box::new(move || control::running(&socket)),
            request_shutdown: Box::new(move || {
                Client::connect(&socket2)
                    .and_then(|mut c| c.call(&Request::Shutdown))
                    .is_ok_and(|r| r.ok)
            }),
            timeout: Duration::from_secs(10),
            poll: Duration::from_millis(200),
        }
    }

    fn wait_for(&self, mut done: impl FnMut() -> Option<Result<(), Failure>>) -> Option<CmdResult> {
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(result) = done() {
                return Some(result);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(self.poll);
        }
    }
}

/// The user behind `sudo`, with their primary group.
fn sudo_user() -> Option<RunAs> {
    let name = std::env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty() && u != "root")?;
    let user = nix::unistd::User::from_name(&name).ok().flatten()?;
    let group = nix::unistd::Group::from_gid(user.gid)
        .ok()
        .flatten()
        .map_or_else(|| user.gid.to_string(), |g| g.name);
    Some(RunAs { user: name, group })
}

fn no_manager() -> Failure {
    Failure::new("no supported service manager was found on this system")
        .hint(format!("Supported: {}.", SUPPORTED.join(", ")))
        .hint("Run proxlet in the foreground instead: proxlet run")
}

fn describe(s: &StatusInfo) -> String {
    if s.run_by == "foreground" {
        format!("in the foreground, pid {}", s.pid)
    } else {
        format!("as a {} service, pid {}", s.run_by, s.pid)
    }
}

/// `proxlet start` on the real host.
pub fn start(locations: &Locations) -> CmdResult {
    start_with(&Host::system(locations), locations)
}

/// `proxlet stop` on the real host.
pub fn stop(locations: &Locations) -> CmdResult {
    stop_with(&Host::system(locations), locations)
}

/// Installs the service, starts it, and waits until it serves.
pub fn start_with(host: &Host, locations: &Locations) -> CmdResult {
    if let Some(s) = (host.probe)() {
        output::success(format!("proxlet is already running ({})", describe(&s)));
        return Ok(());
    }
    let manager = host.manager.as_deref().ok_or_else(no_manager)?;
    if !host.is_root {
        return Err(Failure::new("installing the system service needs root")
            .hint("Run: sudo proxlet start"));
    }
    if host.invoking_user.is_none() {
        output::warn(
            "No sudo user found; the service will run as root and use system directories.",
        );
    }

    let spec = ServiceSpec {
        exec: host.exe.clone(),
        args: vec!["run".into()],
        run_as: host.invoking_user.clone(),
        env: vec![
            (SERVICE_ENV.into(), manager.name().into()),
            (
                "PROXLET_CONFIG_DIR".into(),
                locations.config_dir.display().to_string(),
            ),
            (
                "PROXLET_STATE_DIR".into(),
                locations.state_dir.display().to_string(),
            ),
        ],
        bind_privileged_ports: host.invoking_user.is_some(),
    };
    let service_error = |e: proxlet_service::ServiceError| Failure::new(e.to_string());
    manager.install(&spec).map_err(service_error)?;
    manager.start().map_err(service_error)?;

    let result = host.wait_for(|| {
        if let Some(s) = (host.probe)() {
            if s.mode.contains(crate::ports::CHECKING) {
                return None;
            }
            output::success(format!(
                "proxlet is running as a {} service",
                manager.name()
            ));
            let rules = format!("{} rule{}", s.rules, if s.rules == 1 { "" } else { "s" });
            let port = s.listen.rsplit(':').next().unwrap_or("80");
            if let Some(reason) = s
                .mode
                .strip_prefix(&format!("waiting for port {port} ("))
                .and_then(|r| r.strip_suffix(')'))
            {
                let pid = port.parse().ok().and_then(proxlet_core::port::holder);
                output::warn(format!(
                    "waiting for port {port}, {reason}{}",
                    pid.map(|h| format!(" (pid {})", h.pid)).unwrap_or_default()
                ));
                output::note(format!(
                    "proxlet takes port {port} as soon as it is free · {rules} · starts at boot"
                ));
            } else if s.mode == "standalone" {
                output::note(format!(
                    "listening on http://{} · {rules} · starts at boot",
                    s.listen
                ));
            } else {
                output::note(format!("{} · {rules} · starts at boot", s.mode));
            }
            output::note("Stop it with: sudo proxlet stop");
            return Some(Ok(()));
        }
        match manager.state() {
            Ok(ServiceState::Failed | ServiceState::Restarting) => {
                Some(Err(start_failure(manager)))
            }
            _ => None,
        }
    });
    result.unwrap_or_else(|| {
        Err(Failure::new(format!(
            "proxlet did not start serving within {}s",
            host.timeout.as_secs()
        ))
        .hint(format!("See its log: {}", log_hint(manager))))
    })
}

/// Explains a failed start with the server's own error, and stops the retry loop.
fn start_failure(manager: &dyn ServiceManager) -> Failure {
    let mut logs = manager.recent_logs(30);
    if !logs.iter().any(|l| l.contains("error: ")) {
        // The journal can lag the crash by a moment.
        std::thread::sleep(Duration::from_millis(500));
        logs = manager.recent_logs(30);
    }
    let error_at = logs.iter().rposition(|l| l.contains("error: "));
    let reason = error_at
        .and_then(|i| logs[i].split_once("error: ").map(|(_, msg)| msg.to_owned()))
        .or_else(|| logs.last().cloned())
        .unwrap_or_else(|| "the service exited".into());
    // The daemon prints hints as indented lines right after its error.
    let hints: Vec<String> = error_at
        .map(|i| {
            logs[i + 1..]
                .iter()
                .take_while(|l| l.starts_with("  "))
                .map(|l| l.trim().to_owned())
                .collect()
        })
        .unwrap_or_default();
    let _ = manager.stop();
    let mut failure = Failure::new(format!("proxlet failed to start: {reason}"));
    for hint in hints {
        failure = failure.hint(hint);
    }
    failure.hint(format!("Full log: {}", log_hint(manager)))
}

fn log_hint(manager: &dyn ServiceManager) -> &'static str {
    match manager.name() {
        "systemd" => "journalctl -u proxlet.service",
        _ => "your service manager's log for proxlet",
    }
}

/// Stops the service (or a foreground daemon) and keeps rules and settings.
pub fn stop_with(host: &Host, _locations: &Locations) -> CmdResult {
    let state = match host.manager.as_deref() {
        Some(m) => Some(m.state().map_err(|e| Failure::new(e.to_string()))?),
        None => None,
    };
    let service_active = matches!(
        state,
        Some(
            ServiceState::Running
                | ServiceState::Starting
                | ServiceState::Restarting
                | ServiceState::Failed
        )
    );

    if let (Some(manager), true) = (host.manager.as_deref(), service_active) {
        if !host.is_root {
            return Err(Failure::new("stopping the system service needs root")
                .hint("Run: sudo proxlet stop"));
        }
        manager.stop().map_err(|e| Failure::new(e.to_string()))?;
        return wait_gone(
            host,
            "Stopped proxlet. It will not start at boot; your rules are kept.",
        );
    }

    // An installed but stopped service may still be enabled at boot.
    if let (Some(manager), Some(ServiceState::Stopped), true) =
        (host.manager.as_deref(), state, host.is_root)
    {
        let _ = manager.stop();
    }

    match (host.probe)() {
        Some(s) if s.run_by == "foreground" => {
            if !(host.request_shutdown)() {
                return Err(Failure::new(format!(
                    "proxlet (pid {}) did not accept the stop request",
                    s.pid
                )));
            }
            wait_gone(
                host,
                &format!("Stopped proxlet (foreground, pid {}).", s.pid),
            )
        }
        Some(s) => Err(Failure::new(format!(
            "proxlet is running {}, which this host's service manager does not control",
            describe(&s)
        ))),
        None if host.manager.is_none() => Err(Failure::new("proxlet is not running")
            .hint(format!(
                "No supported service manager was found (supported: {}).",
                SUPPORTED.join(", ")
            ))
            .hint("To run proxlet, use: proxlet run")),
        None => {
            output::success("proxlet is not running");
            Ok(())
        }
    }
}

fn wait_gone(host: &Host, message: &str) -> CmdResult {
    host.wait_for(|| (host.probe)().is_none().then_some(Ok(())))
        .unwrap_or_else(|| {
            Err(Failure::new(
                "proxlet is still running after the stop request",
            ))
        })?;
    output::success(message);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxlet_service::ServiceError;
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::rc::Rc;

    #[derive(Default)]
    struct FakeState {
        calls: RefCell<Vec<String>>,
        state: Cell<Option<ServiceState>>,
        installed: RefCell<Option<ServiceSpec>>,
        logs: RefCell<Vec<String>>,
        running: Cell<bool>,
        /// What the daemon does once started: come up, or crash.
        comes_up: Cell<bool>,
        /// The mode the daemon reports, if not `standalone`.
        mode: RefCell<Option<String>>,
    }

    struct FakeManager(Rc<FakeState>);

    impl ServiceManager for FakeManager {
        fn name(&self) -> &'static str {
            "systemd"
        }
        fn is_present(&self) -> bool {
            true
        }
        fn install(&self, spec: &ServiceSpec) -> Result<bool, ServiceError> {
            self.0.calls.borrow_mut().push("install".into());
            *self.0.installed.borrow_mut() = Some(spec.clone());
            Ok(true)
        }
        fn start(&self) -> Result<(), ServiceError> {
            self.0.calls.borrow_mut().push("start".into());
            if self.0.comes_up.get() {
                self.0.running.set(true);
                self.0.state.set(Some(ServiceState::Running));
            } else {
                self.0.state.set(Some(ServiceState::Restarting));
            }
            Ok(())
        }
        fn stop(&self) -> Result<(), ServiceError> {
            self.0.calls.borrow_mut().push("stop".into());
            self.0.running.set(false);
            self.0.state.set(Some(ServiceState::Stopped));
            Ok(())
        }
        fn state(&self) -> Result<ServiceState, ServiceError> {
            Ok(self.0.state.get().unwrap_or(ServiceState::NotInstalled))
        }
        fn recent_logs(&self, _: usize) -> Vec<String> {
            self.0.logs.borrow().clone()
        }
    }

    fn status(run_by: &str) -> StatusInfo {
        StatusInfo {
            version: "0.1.0".into(),
            pid: 42,
            mode: "standalone".into(),
            listen: "0.0.0.0:80".into(),
            internal: "127.0.0.1:47080".into(),
            run_by: run_by.into(),
            uptime_secs: 1,
            rules: 2,
        }
    }

    fn host(fake: &Rc<FakeState>, is_root: bool, with_manager: bool) -> Host {
        let probe_state = fake.clone();
        let stop_state = fake.clone();
        Host {
            manager: with_manager
                .then(|| Box::new(FakeManager(fake.clone())) as Box<dyn ServiceManager>),
            is_root,
            invoking_user: Some(RunAs {
                user: "alice".into(),
                group: "staff".into(),
            }),
            exe: "/usr/local/bin/proxlet".into(),
            probe: Box::new(move || {
                probe_state.running.get().then(|| StatusInfo {
                    mode: probe_state
                        .mode
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| "standalone".into()),
                    ..status("systemd")
                })
            }),
            request_shutdown: Box::new(move || {
                stop_state.running.set(false);
                true
            }),
            timeout: Duration::from_millis(300),
            poll: Duration::from_millis(10),
        }
    }

    fn locations() -> Locations {
        Locations::for_user_home(Path::new("/home/alice"))
    }

    #[test]
    fn start_on_a_taken_port_succeeds_and_waits_for_it() {
        let fake = Rc::new(FakeState::default());
        fake.comes_up.set(true);
        *fake.mode.borrow_mut() = Some("waiting for port 80 (held by caddy)".into());
        start_with(&host(&fake, true, true), &locations()).unwrap();
        assert_eq!(*fake.calls.borrow(), ["install", "start"]);
    }

    #[test]
    fn start_waits_until_the_port_has_been_checked() {
        let fake = Rc::new(FakeState::default());
        fake.comes_up.set(true);
        *fake.mode.borrow_mut() = Some(format!("waiting for port 80 ({})", crate::ports::CHECKING));
        let err = start_with(&host(&fake, true, true), &locations()).unwrap_err();
        assert!(
            err.message.contains("did not start serving"),
            "{}",
            err.message
        );
    }

    #[test]
    fn first_start_installs_runs_as_invoking_user_and_waits() {
        let fake = Rc::new(FakeState::default());
        fake.comes_up.set(true);
        start_with(&host(&fake, true, true), &locations()).unwrap();
        assert_eq!(*fake.calls.borrow(), ["install", "start"]);
        let spec = fake.installed.borrow().clone().unwrap();
        assert_eq!(spec.args, ["run"]);
        assert_eq!(spec.run_as.unwrap().user, "alice");
        assert!(spec.bind_privileged_ports);
        assert!(
            spec.env
                .contains(&("PROXLET_SERVICE".into(), "systemd".into()))
        );
        assert!(spec.env.contains(&(
            "PROXLET_CONFIG_DIR".into(),
            "/home/alice/.config/proxlet".into()
        )));
        assert!(spec.env.contains(&(
            "PROXLET_STATE_DIR".into(),
            "/home/alice/.local/state/proxlet".into()
        )));
    }

    #[test]
    fn start_when_already_running_changes_nothing() {
        let fake = Rc::new(FakeState::default());
        fake.running.set(true);
        start_with(&host(&fake, true, true), &locations()).unwrap();
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn start_failure_shows_the_servers_error_and_stops_retrying() {
        let fake = Rc::new(FakeState::default());
        *fake.logs.borrow_mut() = vec![
            "proxlet starting".into(),
            "error: port 80 is already in use by apache2 (pid 9)".into(),
            "  Stop whatever holds it, or move proxlet: proxlet config listen.http_port 8080"
                .into(),
        ];
        let err = start_with(&host(&fake, true, true), &locations()).unwrap_err();
        assert_eq!(
            err.message,
            "proxlet failed to start: port 80 is already in use by apache2 (pid 9)"
        );
        assert!(
            err.hints
                .iter()
                .any(|h| h.contains("listen.http_port 8080")),
            "{:?}",
            err.hints
        );
        assert!(err.hints.iter().any(|h| h.contains("journalctl")));
        assert_eq!(*fake.calls.borrow(), ["install", "start", "stop"]);
    }

    #[test]
    fn start_without_a_service_manager_points_to_run() {
        let fake = Rc::new(FakeState::default());
        let err = start_with(&host(&fake, true, false), &locations()).unwrap_err();
        assert!(err.message.contains("no supported service manager"));
        assert!(err.hints.iter().any(|h| h.contains("systemd")));
        assert!(err.hints.iter().any(|h| h.contains("proxlet run")));
    }

    #[test]
    fn start_without_root_asks_for_sudo() {
        let fake = Rc::new(FakeState::default());
        let err = start_with(&host(&fake, false, true), &locations()).unwrap_err();
        assert!(err.hints.iter().any(|h| h.contains("sudo proxlet start")));
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn stop_running_service() {
        let fake = Rc::new(FakeState::default());
        fake.running.set(true);
        fake.state.set(Some(ServiceState::Running));
        stop_with(&host(&fake, true, true), &locations()).unwrap();
        assert_eq!(*fake.calls.borrow(), ["stop"]);
        assert!(!fake.running.get());
    }

    #[test]
    fn stop_when_not_running_succeeds() {
        let fake = Rc::new(FakeState::default());
        stop_with(&host(&fake, true, true), &locations()).unwrap();
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn stop_service_without_root_asks_for_sudo() {
        let fake = Rc::new(FakeState::default());
        fake.running.set(true);
        fake.state.set(Some(ServiceState::Running));
        let err = stop_with(&host(&fake, false, true), &locations()).unwrap_err();
        assert!(err.hints.iter().any(|h| h.contains("sudo proxlet stop")));
    }

    #[test]
    fn stop_foreground_daemon_without_service_manager() {
        let fake = Rc::new(FakeState::default());
        fake.running.set(true);
        let mut h = host(&fake, false, false);
        let probe_state = fake.clone();
        h.probe = Box::new(move || probe_state.running.get().then(|| status("foreground")));
        stop_with(&h, &locations()).unwrap();
        assert!(!fake.running.get());
    }

    #[test]
    fn stop_with_nothing_running_and_no_manager_fails() {
        let fake = Rc::new(FakeState::default());
        let err = stop_with(&host(&fake, false, false), &locations()).unwrap_err();
        assert!(err.hints.iter().any(|h| h.contains("systemd")));
        assert!(err.hints.iter().any(|h| h.contains("proxlet run")));
    }
}
