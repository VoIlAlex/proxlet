//! `proxlet start` and `proxlet stop`.
//!
//! Everything host-specific (the service manager, whether we are root, who
//! invoked `sudo`, how to probe the daemon) is gathered in [`Host`], so the
//! command logic can be tested against a fake manager.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use proxlet_core::{ConfigFile, Locations};
use proxlet_front::FrontServer;
use proxlet_service::{RunAs, SUPPORTED, ServiceManager, ServiceSpec, ServiceState, UnitHook};

use crate::attach::{self, Attach, Attached, Outcome};

use crate::control::{self, Client, Request, StatusInfo};
use crate::daemon::SERVICE_ENV;
use crate::output::{self, CmdResult, Failure};

/// Asks a yes/no question; `None` when there is no terminal to ask on.
pub type Confirm = dyn Fn(&str) -> Option<bool>;

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
    /// The front server to attach behind, if attach is possible here.
    pub front: Option<Box<dyn FrontServer>>,
    /// Asks a yes/no question; `None` when there is no terminal.
    pub confirm: Box<Confirm>,
    /// Where `start` creates the directory nginx's workers read
    /// certificates from; `None` uses proxlet's own store.
    pub publish_dir: Option<PathBuf>,
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
            front: Some(attach::system_front()),
            confirm: Box::new(output::confirm),
            publish_dir: is_root.then(|| PathBuf::from(attach::PUBLISH_DIR)),
        }
    }

    /// Attach and detach for this host, if it has a front server.
    fn attach(&self, locations: &Locations, yes: bool) -> Option<Attach<'_>> {
        let front = self.front.as_deref()?;
        let argv = |action: &str| -> Vec<String> {
            vec![
                self.exe.display().to_string(),
                "handoff".into(),
                action.into(),
                "--socket".into(),
                locations.socket().display().to_string(),
            ]
        };
        Some(Attach {
            front,
            manager: self.manager.as_deref(),
            ledger: locations.state_dir.join(attach::LEDGER),
            owner: self.invoking_user.as_ref().and_then(owner_ids),
            hook: Some(UnitHook {
                before_start: argv("release"),
                after_stop: argv("reclaim"),
            }),
            cert_dir: match &self.publish_dir {
                Some(dir) => attach::CertDir::Create(dir.clone()),
                None => attach::CertDir::Existing(
                    crate::commands::cert::store(locations).dir().to_owned(),
                ),
            },
            yes,
            confirm: &*self.confirm,
        })
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

/// The warning when another process holds the HTTPS port, with `pid` the
/// holder's process id if known.
fn https_waiting(s: &StatusInfo, pid: impl FnOnce(u16) -> Option<u32>) -> Option<String> {
    let port = s.https_listen.rsplit(':').next().unwrap_or("443");
    let reason = s
        .https_mode
        .strip_prefix(&format!("waiting for port {port} ("))
        .and_then(|r| r.strip_suffix(')'))?;
    let pid = port.parse().ok().and_then(pid);
    Some(format!(
        "waiting for port {port}, {reason}{}",
        pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
    ))
}

/// Warns when another process holds the HTTPS port.
fn warn_https_waiting(s: &StatusInfo) {
    if let Some(w) = https_waiting(s, |p| proxlet_core::port::holder(p).map(|h| h.pid)) {
        output::warn(w);
        output::note("HTTPS starts as soon as the port is free; plain HTTP works meanwhile");
    }
}

/// The numeric ids of a service user.
fn owner_ids(run_as: &RunAs) -> Option<(u32, u32)> {
    let user = nix::unistd::User::from_name(&run_as.user).ok().flatten()?;
    Some((user.uid.as_raw(), user.gid.as_raw()))
}

/// Prints what attaching did.
fn report_attach(a: &Attached) {
    match &a.outcome {
        Outcome::Attached {
            wrote,
            moved_default,
            kept_default,
        } => {
            output::success(format!(
                "Attached to {}: requests for hosts it does not serve now reach proxlet",
                a.front
            ));
            if let Some(path) = wrote {
                output::note(format!("Added {}", path.display()));
            }
            for at in moved_default {
                output::note(format!("Moved default_server from {at} to proxlet's block"));
            }
            for at in kept_default {
                output::warn(format!(
                    "{} keeps its own default server at {at}; requests on that port for hosts \
                     it does not serve go there, not to proxlet",
                    a.front
                ));
            }
        }
        Outcome::Declined { default_at } => {
            output::warn(format!(
                "{} keeps its own default server at {default_at}; requests for hosts it does not \
                 serve go there, not to proxlet",
                a.front
            ));
            output::note("To make proxlet the default instead: sudo proxlet start --yes");
        }
        Outcome::AlreadyAttached | Outcome::Disabled | Outcome::NoFront => {}
    }
    if let Some(why) = &a.https_skipped {
        output::warn(format!("HTTPS is not forwarded to proxlet: {why}"));
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
pub fn start(locations: &Locations, yes: bool) -> CmdResult {
    start_with(&Host::system(locations), locations, yes)
}

/// `proxlet stop` on the real host.
pub fn stop(locations: &Locations) -> CmdResult {
    stop_with(&Host::system(locations), locations)
}

/// Installs the service, attaches behind a front server if there is one,
/// starts the service, and waits until it serves.
pub fn start_with(host: &Host, locations: &Locations, yes: bool) -> CmdResult {
    let running = (host.probe)();
    if let Some(s) = &running
        && !host.is_root
    {
        output::success(format!("proxlet is already running ({})", describe(s)));
        return Ok(());
    }
    if running.is_none() {
        host.manager.as_deref().ok_or_else(no_manager)?;
        if !host.is_root {
            return Err(Failure::new("installing the system service needs root")
                .hint("Run: sudo proxlet start"));
        }
    }

    let (_, config) = ConfigFile::new(locations.config_file()).read()?;
    let attach = host.attach(locations, yes);
    let attached = match &attach {
        Some(a) => Some(a.attach(&config.settings)?),
        None => None,
    };
    if let Some(a) = &attached {
        report_attach(a);
    }
    if let Some(s) = running {
        // Let the running server publish certificates for the front server
        // and notice that HTTPS is now forwarded.
        let _ =
            Client::connect(&locations.socket()).and_then(|mut c| c.call(&Request::CertsChanged));
        output::success(format!("proxlet is already running ({})", describe(&s)));
        return Ok(());
    }
    let manager = host.manager.as_deref().ok_or_else(no_manager)?;
    let supplementary_groups = attached
        .as_ref()
        .and_then(|a| a.error_log.as_deref())
        .and_then(attach::log_group)
        .into_iter()
        .collect();
    let result = install_and_wait(host, locations, manager, supplementary_groups);
    if result.is_err()
        && let Some(a) = &attach
    {
        // Don't leave the front server forwarding to a server that is not there.
        let _ = a.detach();
    }
    result
}

fn install_and_wait(
    host: &Host,
    locations: &Locations,
    manager: &dyn ServiceManager,
    supplementary_groups: Vec<String>,
) -> CmdResult {
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
        supplementary_groups,
    };
    let service_error = |e: proxlet_service::ServiceError| Failure::new(e.to_string());
    manager.install(&spec).map_err(service_error)?;
    manager.start().map_err(service_error)?;

    let result = host.wait_for(|| {
        if let Some(s) = (host.probe)() {
            if s.mode.contains(crate::ports::CHECKING)
                || s.https_mode.contains(crate::ports::CHECKING)
            {
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
            warn_https_waiting(&s);
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

/// Stops the service (or a foreground daemon), detaches from the front
/// server, and keeps rules and settings.
pub fn stop_with(host: &Host, locations: &Locations) -> CmdResult {
    stop_server(host)?;
    let Some(attach) = host.attach(locations, false) else {
        return Ok(());
    };
    if let Some(warnings) = attach.detach()? {
        for w in warnings {
            output::warn(w);
        }
        output::success(format!(
            "Detached from {}: its config is as it was before proxlet",
            attach.front.name()
        ));
    }
    Ok(())
}

fn stop_server(host: &Host) -> CmdResult {
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
    pub(super) struct FakeState {
        pub(super) calls: RefCell<Vec<String>>,
        pub(super) state: Cell<Option<ServiceState>>,
        pub(super) installed: RefCell<Option<ServiceSpec>>,
        pub(super) logs: RefCell<Vec<String>>,
        pub(super) running: Cell<bool>,
        /// What the daemon does once started: come up, or crash.
        pub(super) comes_up: Cell<bool>,
        /// The mode the daemon reports, if not `standalone`.
        pub(super) mode: RefCell<Option<String>>,
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
        fn hook(&self, unit: &str, _: &proxlet_service::UnitHook) -> Result<bool, ServiceError> {
            self.0.calls.borrow_mut().push(format!("hook {unit}"));
            Ok(true)
        }
        fn unhook(&self, unit: &str) -> Result<bool, ServiceError> {
            self.0.calls.borrow_mut().push(format!("unhook {unit}"));
            Ok(true)
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
            ..StatusInfo::default()
        }
    }

    pub(super) fn host(fake: &Rc<FakeState>, is_root: bool, with_manager: bool) -> Host {
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
            front: None,
            confirm: Box::new(|_| panic!("unexpected prompt")),
            publish_dir: None,
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
        start_with(&host(&fake, true, true), &locations(), false).unwrap();
        assert_eq!(*fake.calls.borrow(), ["install", "start"]);
    }

    #[test]
    fn start_on_a_taken_https_port_warns_and_succeeds() {
        let s = StatusInfo {
            https_mode: "waiting for port 443 (held by caddy)".into(),
            https_listen: "0.0.0.0:443".into(),
            ..status("systemd")
        };
        assert_eq!(
            https_waiting(&s, |_| Some(4242)).as_deref(),
            Some("waiting for port 443, held by caddy (pid 4242)")
        );
        let standalone = StatusInfo {
            https_mode: "standalone".into(),
            ..s.clone()
        };
        assert_eq!(https_waiting(&standalone, |_| None), None);

        let fake = Rc::new(FakeState::default());
        fake.comes_up.set(true);
        start_with(&host(&fake, true, true), &locations(), false).unwrap();
        assert_eq!(*fake.calls.borrow(), ["install", "start"]);
    }

    #[test]
    fn start_waits_until_the_port_has_been_checked() {
        let fake = Rc::new(FakeState::default());
        fake.comes_up.set(true);
        *fake.mode.borrow_mut() = Some(format!("waiting for port 80 ({})", crate::ports::CHECKING));
        let err = start_with(&host(&fake, true, true), &locations(), false).unwrap_err();
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
        start_with(&host(&fake, true, true), &locations(), false).unwrap();
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
        start_with(&host(&fake, true, true), &locations(), false).unwrap();
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
        let err = start_with(&host(&fake, true, true), &locations(), false).unwrap_err();
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
        let err = start_with(&host(&fake, true, false), &locations(), false).unwrap_err();
        assert!(err.message.contains("no supported service manager"));
        assert!(err.hints.iter().any(|h| h.contains("systemd")));
        assert!(err.hints.iter().any(|h| h.contains("proxlet run")));
    }

    #[test]
    fn start_without_root_asks_for_sudo() {
        let fake = Rc::new(FakeState::default());
        let err = start_with(&host(&fake, false, true), &locations(), false).unwrap_err();
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

#[cfg(test)]
mod attach_tests {
    //! `start` and `stop` with nginx installed: a real nginx config tree in a
    //! temporary directory, read and planned by the real code, with nginx's
    //! commands answered by a fake runner.

    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use super::tests::{FakeState, host};
    use super::*;
    use proxlet_core::command::{CommandOutput, CommandRunner};
    use proxlet_front::Nginx;

    /// Answers `nginx -V`, `-t` (fails if any file under the tree contains
    /// `bogus`) and `-s reload`; nginx is never running.
    struct FakeNginx {
        root: PathBuf,
        /// Report a new nginx built with TLS, which can forward HTTPS.
        tls: bool,
    }

    fn contains_bogus(dir: &Path) -> bool {
        fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
            let p = e.path();
            if p.is_dir() {
                contains_bogus(&p)
            } else {
                fs::read_to_string(&p).is_ok_and(|t| t.contains("bogus"))
            }
        })
    }

    impl CommandRunner for FakeNginx {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let ok = |stderr: String| CommandOutput {
                code: 0,
                stdout: String::new(),
                stderr,
            };
            match (program, args.first().copied()) {
                ("nginx", Some("-V")) if self.tls => Ok(ok(format!(
                    "nginx version: nginx/1.28.0\nconfigure arguments: --prefix={0} --conf-path={0}/nginx.conf --with-http_ssl_module\n",
                    self.root.display()
                ))),
                ("nginx", Some("-V")) => Ok(ok(format!(
                    "nginx version: nginx/1.14.2\nconfigure arguments: --prefix={0} --conf-path={0}/nginx.conf\n",
                    self.root.display()
                ))),
                ("nginx", Some("-t")) if contains_bogus(&self.root) => Ok(CommandOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "nginx: [emerg] unknown directive \"bogus\"\n".into(),
                }),
                ("nginx", _) => Ok(ok(String::new())),
                _ => Ok(CommandOutput {
                    code: 1,
                    ..CommandOutput::default()
                }),
            }
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let f = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            let n = f.nginx_root();
            fs::create_dir_all(n.join("conf.d")).unwrap();
            fs::create_dir_all(n.join("sites-enabled")).unwrap();
            fs::write(
                n.join("nginx.conf"),
                format!(
                    "events {{}}\nhttp {{\n    include {0}/conf.d/*.conf;\n    include {0}/sites-enabled/*;\n}}\n",
                    n.display()
                ),
            )
            .unwrap();
            fs::write(
                n.join("sites-enabled/default"),
                "server {\n    listen 80 default_server;\n    server_name _;\n}\n",
            )
            .unwrap();
            f
        }

        fn nginx_root(&self) -> PathBuf {
            self.dir.path().join("nginx")
        }

        fn locations(&self) -> Locations {
            Locations::for_user_home(&self.dir.path().join("home"))
        }

        /// Every file under the nginx tree with its contents.
        fn snapshot(&self) -> Vec<(PathBuf, String)> {
            fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
                for e in fs::read_dir(dir).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else {
                        out.push((p.clone(), fs::read_to_string(&p).unwrap()));
                    }
                }
            }
            let mut out = Vec::new();
            walk(&self.nginx_root(), &mut out);
            out.sort();
            out
        }

        fn host(
            &self,
            fake: &Rc<FakeState>,
            answer: Option<bool>,
        ) -> (Host, Rc<RefCell<Vec<String>>>) {
            self.host_with(fake, answer, false)
        }

        fn publish_dir(&self) -> PathBuf {
            self.dir.path().join("published")
        }

        /// Like [`Fixture::host`]; with `tls`, nginx can forward HTTPS and
        /// `start` publishes certificates in [`Fixture::publish_dir`].
        fn host_with(
            &self,
            fake: &Rc<FakeState>,
            answer: Option<bool>,
            tls: bool,
        ) -> (Host, Rc<RefCell<Vec<String>>>) {
            let mut h = host(fake, true, true);
            h.front = Some(Box::new(Nginx::with_candidates(
                FakeNginx {
                    root: self.nginx_root(),
                    tls,
                },
                &["nginx"],
            )));
            h.publish_dir = Some(self.publish_dir());
            let asked = Rc::new(RefCell::new(Vec::new()));
            let log = asked.clone();
            h.confirm = Box::new(move |q| {
                log.borrow_mut().push(q.to_owned());
                answer
            });
            (h, asked)
        }
    }

    fn started() -> Rc<FakeState> {
        let fake = Rc::new(FakeState::default());
        fake.comes_up.set(true);
        fake
    }

    #[test]
    fn take_over_with_confirmation_then_stop_restores_everything() {
        let f = Fixture::new();
        let before = f.snapshot();
        let fake = started();
        let (h, asked) = f.host(&fake, Some(true));
        start_with(&h, &f.locations(), false).unwrap();

        assert_eq!(asked.borrow().len(), 1);
        assert!(
            asked.borrow()[0].contains("sites-enabled/default:2"),
            "{:?}",
            asked.borrow()
        );
        assert_eq!(
            fs::read_to_string(f.nginx_root().join("sites-enabled/default")).unwrap(),
            "server {\n    listen 80;\n    server_name _;\n}\n"
        );
        let block = fs::read_to_string(f.nginx_root().join("conf.d/proxlet.conf")).unwrap();
        assert!(block.contains("listen 80 default_server;"), "{block}");
        assert!(
            block.contains("proxy_pass http://127.0.0.1:47080;"),
            "{block}"
        );
        assert_eq!(
            *fake.calls.borrow(),
            ["hook nginx.service", "install", "start"]
        );
        let spec = fake.installed.borrow().clone().unwrap();
        assert_eq!(spec.supplementary_groups, Vec::<String>::new());

        stop_with(&h, &f.locations()).unwrap();
        assert_eq!(f.snapshot(), before);
        assert!(
            fake.calls
                .borrow()
                .contains(&"unhook nginx.service".to_owned())
        );
        assert!(!f.locations().state_dir.join(attach::LEDGER).exists());
    }

    #[test]
    fn take_over_without_a_prompt() {
        let f = Fixture::new();
        let fake = started();
        let (h, asked) = f.host(&fake, None);
        start_with(&h, &f.locations(), true).unwrap();
        assert!(asked.borrow().is_empty());
        assert!(f.nginx_root().join("conf.d/proxlet.conf").exists());
    }

    #[test]
    fn https_block_and_publish_dir_with_a_tls_nginx_then_stop_removes_them() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let before = f.snapshot();
        let fake = started();
        let (h, _) = f.host_with(&fake, None, true);
        start_with(&h, &f.locations(), true).unwrap();

        let block = fs::read_to_string(f.nginx_root().join("conf.d/proxlet.conf")).unwrap();
        assert!(block.contains("listen 443 ssl default_server;"), "{block}");
        assert!(
            block.contains(&format!(
                "ssl_certificate     {}/$proxlet_cert.pem;",
                f.publish_dir().display()
            )),
            "{block}"
        );
        let mode = fs::metadata(f.publish_dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o2750, "{mode:o}");
        let ledger = proxlet_front::Ledger::load(&f.locations().state_dir.join(attach::LEDGER))
            .unwrap()
            .unwrap();
        assert_eq!(ledger.https_port, Some(443));
        assert!(ledger.cert_dir_created);

        stop_with(&h, &f.locations()).unwrap();
        assert_eq!(f.snapshot(), before);
        assert!(
            !f.publish_dir().exists(),
            "the publish directory is removed"
        );
    }

    #[test]
    fn old_nginx_gets_only_the_http_block_and_a_warning() {
        let f = Fixture::new();
        let fake = started();
        let (h, _) = f.host_with(&fake, None, false);
        let a = h.attach(&f.locations(), true).unwrap();
        let attached = a.attach(&proxlet_core::Settings::default()).unwrap();
        let why = attached.https_skipped.unwrap();
        assert!(
            why.contains("nginx 1.15.9 or newer") && why.contains("1.14.2"),
            "{why}"
        );
        let block = fs::read_to_string(f.nginx_root().join("conf.d/proxlet.conf")).unwrap();
        assert!(!block.contains("443"), "{block}");
        assert!(!f.publish_dir().exists());
    }

    #[test]
    fn one_question_names_the_default_servers_of_both_ports() {
        let f = Fixture::new();
        fs::write(
            f.nginx_root().join("sites-enabled/main"),
            "server {\n    server_name main.example;\n    listen 443 ssl default_server;\n}\n",
        )
        .unwrap();
        let fake = started();
        let (h, asked) = f.host_with(&fake, Some(true), true);
        start_with(&h, &f.locations(), false).unwrap();
        let asked = asked.borrow();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(
            asked[0].contains("sites-enabled/default:2")
                && asked[0].contains("sites-enabled/main:3"),
            "{asked:?}"
        );
        assert_eq!(
            fs::read_to_string(f.nginx_root().join("sites-enabled/main")).unwrap(),
            "server {\n    server_name main.example;\n    listen 443 ssl;\n}\n"
        );
    }

    #[test]
    fn declining_keeps_both_defaults_and_writes_nothing() {
        let f = Fixture::new();
        fs::write(
            f.nginx_root().join("sites-enabled/main"),
            "server {\n    server_name main.example;\n    listen 443 ssl default_server;\n}\n",
        )
        .unwrap();
        let before = f.snapshot();
        let fake = started();
        let (h, _) = f.host_with(&fake, Some(false), true);
        start_with(&h, &f.locations(), false).unwrap();
        assert_eq!(f.snapshot(), before);
    }

    #[test]
    fn declined_or_no_terminal_leaves_nginx_config_unchanged() {
        for answer in [Some(false), None] {
            let f = Fixture::new();
            let before = f.snapshot();
            let fake = started();
            let (h, _) = f.host(&fake, answer);
            start_with(&h, &f.locations(), false).unwrap();
            assert_eq!(f.snapshot(), before, "answer {answer:?}");
            // The hook is still installed, so handing the port over works.
            assert_eq!(
                *fake.calls.borrow(),
                ["hook nginx.service", "install", "start"]
            );
        }
    }

    #[test]
    fn nginx_rejecting_the_change_leaves_everything_as_it_was() {
        let f = Fixture::new();
        fs::write(f.nginx_root().join("conf.d/broken.conf"), "bogus;\n").unwrap();
        let before = f.snapshot();
        let fake = started();
        let (h, _) = f.host(&fake, Some(true));
        let err = start_with(&h, &f.locations(), false).unwrap_err();
        assert!(err.message.contains("nginx rejected"), "{}", err.message);
        assert!(
            err.hints
                .iter()
                .any(|h| h.contains("unknown directive \"bogus\"")),
            "{:?}",
            err.hints
        );
        assert_eq!(f.snapshot(), before);
        assert!(fake.calls.borrow().is_empty(), "{:?}", fake.calls.borrow());
        assert!(!f.locations().state_dir.join(attach::LEDGER).exists());
    }

    #[test]
    fn setting_up_the_attach_while_already_running() {
        let f = Fixture::new();
        let fake = started();
        fake.running.set(true);
        let (h, _) = f.host(&fake, Some(true));
        start_with(&h, &f.locations(), false).unwrap();
        assert!(f.nginx_root().join("conf.d/proxlet.conf").exists());
        // Nothing is reinstalled or restarted.
        assert_eq!(*fake.calls.borrow(), ["hook nginx.service"]);
        // Running it again changes nothing.
        start_with(&h, &f.locations(), false).unwrap();
        assert_eq!(
            *fake.calls.borrow(),
            ["hook nginx.service", "hook nginx.service"]
        );
    }

    #[test]
    fn attach_disabled_touches_nothing() {
        let f = Fixture::new();
        let before = f.snapshot();
        let loc = f.locations();
        fs::create_dir_all(&loc.config_dir).unwrap();
        fs::write(loc.config_file(), "[attach]\nenabled = false\n").unwrap();
        let fake = started();
        let (h, _) = f.host(&fake, Some(true));
        start_with(&h, &loc, false).unwrap();
        assert_eq!(f.snapshot(), before);
        assert_eq!(*fake.calls.borrow(), ["install", "start"]);
    }

    #[test]
    fn a_failed_start_detaches_again() {
        let f = Fixture::new();
        let before = f.snapshot();
        let fake = Rc::new(FakeState::default());
        fake.comes_up.set(false);
        let (h, _) = f.host(&fake, Some(true));
        assert!(start_with(&h, &f.locations(), false).is_err());
        assert_eq!(f.snapshot(), before);
    }
}
