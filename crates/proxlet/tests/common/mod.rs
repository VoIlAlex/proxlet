//! Helpers for driving the `proxlet` binary in an isolated `PROXLET_HOME`.
#![allow(dead_code, missing_docs)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_proxlet");

/// An isolated proxlet home. Lives under `/tmp` so socket paths stay short.
pub struct Home {
    pub dir: tempfile::TempDir,
    pub port: u16,
    /// The internal (attach) listener's port.
    pub internal_port: u16,
    pub https_port: u16,
    /// A fake `nginx` script, if this home has one.
    pub nginx: Option<PathBuf>,
}

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    fn from(out: &Output) -> Self {
        Self {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    pub fn ok(&self) -> &Self {
        assert_eq!(
            self.code, 0,
            "stdout:\n{}\nstderr:\n{}",
            self.stdout, self.stderr
        );
        self
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Home {
    /// A home whose server listens on a free loopback port.
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("pxt")
            .tempdir_in("/tmp")
            .unwrap();
        let home = Self {
            dir,
            port: free_port(),
            internal_port: free_port(),
            https_port: free_port(),
            nginx: None,
        };
        home.run(&["config", "listen.address", "127.0.0.1"]).ok();
        home.run(&["config", "listen.http_port", &home.port.to_string()])
            .ok();
        home.run(&["config", "attach.port", &home.internal_port.to_string()])
            .ok();
        home.run(&["config", "listen.https_port", &home.https_port.to_string()])
            .ok();
        home
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn config_file(&self) -> PathBuf {
        self.path().join("proxlet.toml")
    }

    pub fn socket(&self) -> PathBuf {
        self.path().join("state/proxlet.sock")
    }

    pub fn command(&self, args: &[&str]) -> Command {
        self.command_as(Path::new(BIN), args)
    }

    /// Runs a copy of the binary (e.g. one named `nginx`).
    pub fn command_as(&self, bin: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(bin);
        // Never touch a real nginx on the machine running the tests.
        let nginx = self
            .nginx
            .clone()
            .unwrap_or_else(|| PathBuf::from("/nonexistent/nginx"));
        c.args(args)
            .env("PROXLET_NGINX", nginx)
            .env("PROXLET_HOME", self.path())
            .env("NO_COLOR", "1")
            .env_remove("COMPLETE")
            .current_dir(self.path());
        c
    }

    pub fn run(&self, args: &[&str]) -> Run {
        Run::from(&self.command(args).output().unwrap())
    }

    /// Starts `proxlet run` and waits until it answers `status`.
    pub fn start(&self) -> Daemon {
        self.start_as(Path::new(BIN))
    }

    /// Like [`Home::start`], running `bin` instead of the proxlet binary.
    pub fn start_as(&self, bin: &Path) -> Daemon {
        let child = self
            .command_as(bin, &["run"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut daemon = Daemon {
            child: Some(child),
            log: Arc::default(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.run(&["status"]).code != 0 {
            let child = daemon.child.as_mut().unwrap();
            if let Some(status) = child.try_wait().unwrap() {
                let mut err = String::new();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut err)
                    .unwrap();
                panic!("proxlet run exited early ({status}):\n{err}");
            }
            assert!(
                Instant::now() < deadline,
                "proxlet run did not become ready"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let stderr = daemon.child.as_mut().unwrap().stderr.take().unwrap();
        let log = daemon.log.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { return };
                let mut log = log.lock().unwrap();
                log.push_str(&line);
                log.push('\n');
            }
        });
        daemon
    }

    /// Sends `GET path` to the internal (front-server) listener.
    pub fn get_internal(&self, host: &str, path: &str) -> (u16, String) {
        http_get(self.internal_port, host, path)
    }

    /// Polls `status` until its output contains `needle`, and returns it.
    pub fn wait_for_status(&self, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let out = self.run(&["status"]).stdout;
            if out.contains(needle) {
                return out;
            }
            assert!(
                Instant::now() < deadline,
                "status never showed {needle:?}:\n{out}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Sends `GET path` with `Host: host` to the server and returns (status, body).
    pub fn get(&self, host: &str, path: &str) -> (u16, String) {
        http_get(self.port, host, path)
    }

    /// Runs completion as zsh would for `proxlet <words...>`, the last word being completed.
    pub fn complete(&self, words: &[&str]) -> Vec<String> {
        let mut args = vec!["--", "proxlet"];
        args.extend_from_slice(words);
        let out = self
            .command(&args)
            .env("COMPLETE", "zsh")
            .env("_CLAP_COMPLETE_INDEX", words.len().to_string())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.split(':').next().unwrap_or("").to_owned())
            .filter(|v| !v.starts_with("--"))
            .collect()
    }
}

impl Home {
    /// A home with a fake nginx: a config tree under `nginx/` and a script
    /// that answers `-V`, `-t` (failing on the word `bogus`) and `-s reload`.
    /// `site.example` is served on this home's HTTP port; with `default`,
    /// another server is marked `default_server` for it.
    pub fn with_nginx(default: bool) -> Self {
        let mut home = Self::new();
        let root = home.path().join("nginx");
        std::fs::create_dir_all(root.join("conf.d")).unwrap();
        std::fs::create_dir_all(root.join("sites-enabled")).unwrap();
        std::fs::write(
            root.join("nginx.conf"),
            format!(
                "events {{}}\nhttp {{\n    include {0}/conf.d/*.conf;\n    include {0}/sites-enabled/*;\n}}\n",
                root.display()
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("sites-enabled/site"),
            format!(
                "server {{\n    listen {};\n    server_name site.example;\n}}\n",
                home.port
            ),
        )
        .unwrap();
        if default {
            std::fs::write(
                root.join("sites-enabled/default"),
                format!(
                    "server {{\n    listen {} default_server;\n    server_name _;\n}}\n",
                    home.port
                ),
            )
            .unwrap();
        }
        let script = root.join("nginx");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 ROOT='{0}'\n\
                 case \"$1\" in\n\
                 -V) echo 'nginx version: nginx/1.24.0' >&2\n\
                 \x20   echo \"configure arguments: --prefix=$ROOT --conf-path=$ROOT/nginx.conf --error-log-path=$ROOT/error.log\" >&2 ;;\n\
                 -t) if grep -rqs bogus \"$ROOT/conf.d\" \"$ROOT/sites-enabled\"; then\n\
                 \x20     echo 'nginx: [emerg] unknown directive \"bogus\"' >&2; exit 1\n\
                 \x20   fi ;;\n\
                 esac\n\
                 exit 0\n",
                root.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &script,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        home.nginx = Some(script);
        home
    }

    /// Every file under the fake nginx tree (except the script), with contents.
    pub fn nginx_tree(&self) -> Vec<(PathBuf, String)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().is_some_and(|n| n != "nginx") {
                    out.push((p.clone(), std::fs::read_to_string(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.path().join("nginx"), &mut out);
        out.sort();
        out
    }

    /// A copy of the proxlet binary whose process is named `nginx`.
    pub fn binary_named_nginx(&self) -> PathBuf {
        let dir = self.path().join("fake-bin");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nginx");
        if std::fs::hard_link(BIN, &path).is_err() {
            std::fs::copy(BIN, &path).unwrap();
        }
        path
    }
}

pub fn http_get(port: u16, host: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    let status = buf[9..12].parse().unwrap();
    let body = buf
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

pub struct Daemon {
    child: Option<Child>,
    log: Arc<Mutex<String>>,
}

impl Daemon {
    /// Waits until the daemon's log contains `needle`, and returns the log so far.
    pub fn wait_for_log(&self, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            let log = self.log.lock().unwrap().clone();
            if log.contains(needle) {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "log never showed {needle:?}:\n{log}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// Sends SIGTERM and returns the exit code.
    pub fn stop(self) -> i32 {
        self.signal(nix::sys::signal::Signal::SIGTERM)
    }

    /// Sends SIGINT (what Ctrl-C sends) and returns the exit code.
    pub fn interrupt(self) -> i32 {
        self.signal(nix::sys::signal::Signal::SIGINT)
    }

    fn signal(mut self, signal: nix::sys::signal::Signal) -> i32 {
        let mut child = self.child.take().unwrap();
        send(&child, signal);
        child.wait().unwrap().code().unwrap_or(-1)
    }
}

fn send(child: &Child, signal: nix::sys::signal::Signal) {
    let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let _ = nix::sys::signal::kill(pid, signal);
}

fn terminate(child: &Child) {
    send(child, nix::sys::signal::Signal::SIGTERM);
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate(&child);
            let _ = child.wait();
        }
    }
}
