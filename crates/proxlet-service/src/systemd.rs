//! systemd: a unit in `/etc/systemd/system`, driven with `systemctl`.
//!
//! Only documented exit codes and `--property` queries are used, never
//! human-readable output.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::{CommandRunner, ServiceError, ServiceManager, ServiceSpec, ServiceState};

/// The unit name.
pub const UNIT: &str = "proxlet.service";

/// The systemd service manager.
#[derive(Debug)]
pub struct Systemd<R> {
    runner: R,
    unit_dir: PathBuf,
    booted_marker: PathBuf,
}

impl<R: CommandRunner> Systemd<R> {
    /// systemd on this host.
    pub fn new(runner: R) -> Self {
        Self::with_paths(runner, "/etc/systemd/system", "/run/systemd/system")
    }

    /// systemd with custom locations, for tests.
    pub fn with_paths(
        runner: R,
        unit_dir: impl Into<PathBuf>,
        booted_marker: impl Into<PathBuf>,
    ) -> Self {
        Self {
            runner,
            unit_dir: unit_dir.into(),
            booted_marker: booted_marker.into(),
        }
    }

    /// Where the unit file is written.
    #[must_use]
    pub fn unit_path(&self) -> PathBuf {
        self.unit_dir.join(UNIT)
    }

    fn systemctl(&self, args: &[&str]) -> Result<String, ServiceError> {
        let command = format!("systemctl {}", args.join(" "));
        let out = self
            .runner
            .run("systemctl", args)
            .map_err(|e| ServiceError::Command {
                command: command.clone(),
                message: e.to_string(),
            })?;
        if out.code != 0 {
            let message = if out.stderr.trim().is_empty() {
                format!("exit status {}", out.code)
            } else {
                out.stderr.trim().to_owned()
            };
            return Err(ServiceError::Command { command, message });
        }
        Ok(out.stdout)
    }
}

impl<R: CommandRunner> ServiceManager for Systemd<R> {
    fn name(&self) -> &'static str {
        "systemd"
    }

    fn is_present(&self) -> bool {
        // The check sd_booted(3) performs.
        self.booted_marker.is_dir()
    }

    fn install(&self, spec: &ServiceSpec) -> Result<bool, ServiceError> {
        let unit = render_unit(spec);
        let path = self.unit_path();
        if std::fs::read_to_string(&path).is_ok_and(|current| current == unit) {
            return Ok(false);
        }
        write_atomic(&path, &unit)
            .map_err(|e| ServiceError::Io(format!("cannot write {}: {e}", path.display())))?;
        self.systemctl(&["daemon-reload"])?;
        Ok(true)
    }

    fn start(&self) -> Result<(), ServiceError> {
        self.systemctl(&["enable", "--now", UNIT]).map(drop)
    }

    fn stop(&self) -> Result<(), ServiceError> {
        self.systemctl(&["disable", "--now", UNIT]).map(drop)
    }

    fn state(&self) -> Result<ServiceState, ServiceError> {
        let out = self.systemctl(&["show", UNIT, "--property=LoadState,ActiveState,SubState"])?;
        Ok(parse_state(&out))
    }

    fn recent_logs(&self, lines: usize) -> Vec<String> {
        let n = lines.to_string();
        self.runner
            .run(
                "journalctl",
                &[
                    "--unit",
                    UNIT,
                    "--lines",
                    &n,
                    "--no-pager",
                    "--output",
                    "cat",
                ],
            )
            .map(|o| o.stdout.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

/// Interprets `systemctl show --property=LoadState,ActiveState,SubState`.
fn parse_state(show: &str) -> ServiceState {
    let get = |key: &str| {
        show.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .unwrap_or("")
    };
    match (get("LoadState"), get("ActiveState"), get("SubState")) {
        ("not-found", _, _) => ServiceState::NotInstalled,
        (_, _, "auto-restart") => ServiceState::Restarting,
        (_, "active" | "reloading", _) => ServiceState::Running,
        (_, "activating", _) => ServiceState::Starting,
        (_, "failed", _) => ServiceState::Failed,
        _ => ServiceState::Stopped,
    }
}

/// Quotes a value for a unit file directive (systemd.syntax(7)).
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The unit file for `spec`.
#[must_use]
pub fn render_unit(spec: &ServiceSpec) -> String {
    let mut exec = quote(&spec.exec.to_string_lossy());
    for arg in &spec.args {
        exec.push(' ');
        exec.push_str(&quote(arg));
    }
    let mut unit = String::from(
        "# Installed by `proxlet start`; it is rewritten on every start.\n\
         [Unit]\n\
         Description=proxlet reverse proxy and HTTP server\n\
         Documentation=https://github.com/VoIlAlex/proxlet\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n",
    );
    let _ = writeln!(unit, "ExecStart={exec}");
    if let Some(run_as) = &spec.run_as {
        let _ = writeln!(unit, "User={}", run_as.user);
        let _ = writeln!(unit, "Group={}", run_as.group);
    }
    for (k, v) in &spec.env {
        let _ = writeln!(unit, "Environment={}", quote(&format!("{k}={v}")));
    }
    if spec.bind_privileged_ports {
        unit.push_str("AmbientCapabilities=CAP_NET_BIND_SERVICE\n");
        unit.push_str("CapabilityBoundingSet=CAP_NET_BIND_SERVICE\n");
    }
    unit.push_str(
        "NoNewPrivileges=true\n\
         Restart=on-failure\n\
         RestartSec=2s\n\
         TimeoutStopSec=15s\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
    );
    unit
}

fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("service.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandOutput, RunAs};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Records invocations and replays canned outputs.
    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        replies: RefCell<VecDeque<CommandOutput>>,
    }

    impl Recorder {
        fn reply(&self, code: i32, stdout: &str, stderr: &str) {
            self.replies.borrow_mut().push_back(CommandOutput {
                code,
                stdout: stdout.into(),
                stderr: stderr.into(),
            });
        }
    }

    impl CommandRunner for &Recorder {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            Ok(self.replies.borrow_mut().pop_front().unwrap_or_default())
        }
    }

    fn spec() -> ServiceSpec {
        ServiceSpec {
            exec: "/home/alice/.cargo/bin/proxlet".into(),
            args: vec!["run".into()],
            run_as: Some(RunAs {
                user: "alice".into(),
                group: "alice".into(),
            }),
            env: vec![
                ("PROXLET_SERVICE".into(), "systemd".into()),
                (
                    "PROXLET_CONFIG_DIR".into(),
                    "/home/alice/.config/proxlet".into(),
                ),
            ],
            bind_privileged_ports: true,
        }
    }

    #[test]
    fn unit_file() {
        let unit = render_unit(&spec());
        for line in [
            "ExecStart=\"/home/alice/.cargo/bin/proxlet\" \"run\"",
            "User=alice",
            "Group=alice",
            "Environment=\"PROXLET_SERVICE=systemd\"",
            "Environment=\"PROXLET_CONFIG_DIR=/home/alice/.config/proxlet\"",
            "AmbientCapabilities=CAP_NET_BIND_SERVICE",
            "Restart=on-failure",
            "WantedBy=multi-user.target",
        ] {
            assert!(
                unit.lines().any(|l| l == line),
                "missing {line:?} in:\n{unit}"
            );
        }
    }

    #[test]
    fn unit_file_as_root_has_no_user() {
        let mut s = spec();
        s.run_as = None;
        s.bind_privileged_ports = false;
        let unit = render_unit(&s);
        assert!(!unit.contains("User="));
        assert!(!unit.contains("AmbientCapabilities"));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(r#"a "b" \c 100%"#), r#""a \"b\" \\c 100%%""#);
    }

    #[test]
    fn install_writes_unit_and_reloads_once() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Recorder::default();
        let sd = Systemd::with_paths(&rec, dir.path(), dir.path());
        assert!(sd.install(&spec()).unwrap());
        assert_eq!(
            std::fs::read_to_string(sd.unit_path()).unwrap(),
            render_unit(&spec())
        );
        assert!(!sd.install(&spec()).unwrap(), "unchanged unit rewritten");
        assert_eq!(*rec.calls.borrow(), ["systemctl daemon-reload"]);
    }

    #[test]
    fn start_and_stop_commands() {
        let rec = Recorder::default();
        let sd = Systemd::with_paths(&rec, "/x", "/x");
        sd.start().unwrap();
        sd.stop().unwrap();
        assert_eq!(
            *rec.calls.borrow(),
            [
                "systemctl enable --now proxlet.service",
                "systemctl disable --now proxlet.service"
            ]
        );
    }

    #[test]
    fn command_failure_carries_stderr() {
        let rec = Recorder::default();
        rec.reply(1, "", "Access denied\n");
        let err = Systemd::with_paths(&rec, "/x", "/x").start().unwrap_err();
        assert_eq!(
            err.to_string(),
            "`systemctl enable --now proxlet.service` failed: Access denied"
        );
    }

    #[test]
    fn states() {
        let cases = [
            (
                "LoadState=not-found\nActiveState=inactive\nSubState=dead\n",
                ServiceState::NotInstalled,
            ),
            (
                "LoadState=loaded\nActiveState=active\nSubState=running\n",
                ServiceState::Running,
            ),
            (
                "LoadState=loaded\nActiveState=activating\nSubState=auto-restart\n",
                ServiceState::Restarting,
            ),
            (
                "LoadState=loaded\nActiveState=activating\nSubState=start\n",
                ServiceState::Starting,
            ),
            (
                "LoadState=loaded\nActiveState=failed\nSubState=failed\n",
                ServiceState::Failed,
            ),
            (
                "LoadState=loaded\nActiveState=inactive\nSubState=dead\n",
                ServiceState::Stopped,
            ),
        ];
        for (text, want) in cases {
            assert_eq!(parse_state(text), want, "{text}");
        }
    }

    #[test]
    fn recent_logs_come_from_the_journal() {
        let rec = Recorder::default();
        rec.reply(
            0,
            "line one\nerror: port 80 is already in use by apache2 (pid 9)\n",
            "",
        );
        let logs = Systemd::with_paths(&rec, "/x", "/x").recent_logs(20);
        assert_eq!(logs.len(), 2);
        assert_eq!(
            *rec.calls.borrow(),
            ["journalctl --unit proxlet.service --lines 20 --no-pager --output cat"]
        );
    }

    #[test]
    fn presence_follows_the_booted_marker() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Recorder::default();
        assert!(Systemd::with_paths(&rec, "/x", dir.path()).is_present());
        assert!(!Systemd::with_paths(&rec, "/x", dir.path().join("nope")).is_present());
    }
}
