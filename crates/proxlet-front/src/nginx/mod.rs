//! nginx: the first front server.

mod parse;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use proxlet_core::command::{CommandRunner, SystemRunner};
use proxlet_core::port;

use crate::apply::Step;
use crate::config::FrontConfig;
use crate::{FrontError, FrontServer, Installation, Plan};

pub use parse::MANAGED_MARKER;

/// Where an nginx binary is looked for, in order. `nginx` alone means `$PATH`.
pub const CANDIDATES: &[&str] = &[
    "nginx",
    "/usr/sbin/nginx",
    "/usr/local/sbin/nginx",
    "/usr/local/bin/nginx",
    "/opt/homebrew/bin/nginx",
    "/usr/local/nginx/sbin/nginx",
];

/// nginx, driven through an injectable [`CommandRunner`].
#[derive(Debug, Clone)]
pub struct Nginx<R = SystemRunner> {
    runner: R,
    candidates: Vec<String>,
}

impl Default for Nginx {
    fn default() -> Self {
        Self::new(SystemRunner)
    }
}

impl<R: CommandRunner> Nginx<R> {
    /// nginx found through `runner`, trying [`CANDIDATES`].
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            candidates: CANDIDATES.iter().map(|c| (*c).to_owned()).collect(),
        }
    }

    /// nginx at exactly these binaries (for tests).
    pub fn with_candidates(runner: R, candidates: &[&str]) -> Self {
        Self {
            runner,
            candidates: candidates.iter().map(|c| (*c).to_owned()).collect(),
        }
    }

    fn run(&self, inst: &Installation, args: &[&str]) -> Result<(), FrontError> {
        let binary = inst.binary.display().to_string();
        let command = format!("{binary} {}", args.join(" "));
        let out = self
            .runner
            .run(&binary, args)
            .map_err(|e| FrontError::Command {
                command: command.clone(),
                output: e.to_string(),
            })?;
        if out.code == 0 {
            return Ok(());
        }
        Err(FrontError::Command {
            command,
            output: format!("{}{}", out.stderr, out.stdout),
        })
    }
}

/// Parses `nginx -V` output.
#[must_use]
pub fn parse_version_info(binary: &Path, text: &str) -> Option<Installation> {
    let version = text
        .lines()
        .find_map(|l| l.strip_prefix("nginx version: "))?
        .split_whitespace()
        .next()?
        .rsplit('/')
        .next()?
        .to_owned();
    let args = text
        .lines()
        .find_map(|l| l.strip_prefix("configure arguments:"))
        .unwrap_or("");
    let arg = |name: &str| {
        args.split_whitespace()
            .find_map(|a| a.strip_prefix(name))
            .map(PathBuf::from)
    };
    let prefix = arg("--prefix=").unwrap_or_else(|| "/usr/local/nginx".into());
    let under_prefix = |p: PathBuf| if p.is_absolute() { p } else { prefix.join(p) };
    let conf_path = under_prefix(arg("--conf-path=").unwrap_or_else(|| "conf/nginx.conf".into()));
    let default_error_log =
        under_prefix(arg("--error-log-path=").unwrap_or_else(|| "logs/error.log".into()));
    Some(Installation {
        binary: binary.to_owned(),
        version,
        conf_path,
        prefix,
        default_error_log: Some(default_error_log),
    })
}

/// Renders proxlet's catch-all server block.
#[must_use]
pub fn render(port: u16, internal: SocketAddr, ipv6: bool) -> String {
    let v6 = if ipv6 {
        format!("    listen [::]:{port} default_server;\n")
    } else {
        String::new()
    };
    format!(
        "{MANAGED_MARKER} Removed by `sudo proxlet stop`.
# Every request nginx does not claim goes to proxlet.
map $http_upgrade $proxlet_connection {{
    default upgrade;
    ''      close;
}}

server {{
    listen {port} default_server;
{v6}    server_name _;

    location / {{
        proxy_pass http://{internal};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $proxlet_connection;
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 1h;
    }}
}}
"
    )
}

/// Removes the default-server flag (`default_server` or `default`) from one line.
fn without_flag(line: &str, flag: &str) -> Option<String> {
    let mut out = String::with_capacity(line.len());
    let mut found = false;
    let mut rest = line;
    while let Some(i) = rest.find(flag) {
        let before = &rest[..i];
        let after = &rest[i + flag.len()..];
        let bounded_left = before.ends_with(|c: char| c.is_whitespace());
        let bounded_right =
            after.is_empty() || after.starts_with(|c: char| c.is_whitespace() || c == ';');
        if !found && bounded_left && bounded_right {
            out.push_str(before.trim_end_matches([' ', '\t']));
            rest = after;
            found = true;
        } else {
            out.push_str(&rest[..i + flag.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    found.then_some(out)
}

#[cfg(target_os = "linux")]
fn process_running(_: &dyn CommandRunner) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
            && std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "nginx")
    })
}

#[cfg(not(target_os = "linux"))]
fn process_running(runner: &dyn CommandRunner) -> bool {
    runner
        .run("pgrep", &["-x", "nginx"])
        .is_ok_and(|o| o.code == 0)
}

impl<R: CommandRunner + Send + Sync> FrontServer for Nginx<R> {
    fn name(&self) -> &'static str {
        "nginx"
    }

    fn detect(&self) -> Option<Installation> {
        self.candidates.iter().find_map(|c| {
            let out = self.runner.run(c, &["-V"]).ok()?;
            if out.code != 0 {
                return None;
            }
            // nginx prints -V to stderr.
            parse_version_info(Path::new(c), &format!("{}{}", out.stderr, out.stdout))
        })
    }

    fn is_running(&self) -> bool {
        process_running(&self.runner)
    }

    fn service_unit(&self) -> Option<&'static str> {
        Some("nginx.service")
    }

    fn read(&self, inst: &Installation) -> Result<FrontConfig, FrontError> {
        parse::load(&inst.conf_path)
    }

    fn plan_attach(
        &self,
        cfg: &FrontConfig,
        port: u16,
        internal: SocketAddr,
        take_default: bool,
    ) -> Result<Plan, FrontError> {
        let defaults: Vec<_> = cfg
            .servers
            .iter()
            .filter(|s| !s.managed)
            .flat_map(|s| s.listens.iter())
            .filter(|l| l.port == port && l.default_flag.is_some())
            .collect();
        if !defaults.is_empty() && !take_default {
            return Ok(Plan::default());
        }
        let dir = cfg.http_includes.first().ok_or_else(|| {
            FrontError::Layout(format!(
                "nginx's config ({}) includes no directory inside `http` that proxlet could add a file to; \
                 add a line such as `include /etc/nginx/conf.d/*.conf;` to its `http` block",
                cfg.conf_path.display()
            ))
        })?;

        let mut steps = Vec::new();
        for listen in defaults {
            let path = std::fs::canonicalize(&listen.at.file).map_err(|source| FrontError::Io {
                path: listen.at.file.clone(),
                source,
            })?;
            let text = std::fs::read_to_string(&path).map_err(|source| FrontError::Io {
                path: path.clone(),
                source,
            })?;
            let from = text
                .lines()
                .nth(listen.at.line - 1)
                .unwrap_or_default()
                .to_owned();
            let flag = listen.default_flag.as_deref().unwrap_or("default_server");
            let to = without_flag(&from, flag).ok_or_else(|| {
                FrontError::Layout(format!(
                    "{}: the `{flag}` flag is not on the same line as `listen`; move it there or remove it by hand",
                    listen.at
                ))
            })?;
            steps.push(Step::EditLine {
                path,
                line: listen.at.line,
                from,
                to,
            });
        }
        let ipv6 = cfg.servers.iter().flat_map(|s| &s.listens).any(|l| l.ipv6);
        steps.push(Step::Write {
            path: dir.dir.join(dir.file_name()),
            contents: render(port, internal, ipv6),
        });
        Ok(Plan { steps })
    }

    fn check(&self, inst: &Installation) -> Result<(), FrontError> {
        let conf = inst.conf_path.display().to_string();
        self.run(inst, &["-t", "-q", "-c", &conf])
    }

    fn reload(&self, inst: &Installation) -> Result<(), FrontError> {
        let conf = inst.conf_path.display().to_string();
        self.run(inst, &["-s", "reload", "-c", &conf])
    }

    fn error_log(&self, inst: &Installation, cfg: &FrontConfig) -> Option<PathBuf> {
        match cfg.error_log.as_deref() {
            Some(p) if p == "stderr" || p.starts_with("syslog:") || p.starts_with("memory:") => {
                None
            }
            Some(p) => {
                let p = Path::new(p);
                Some(if p.is_absolute() {
                    p.to_owned()
                } else {
                    inst.prefix.join(p)
                })
            }
            None => inst.default_error_log.clone(),
        }
    }

    fn is_bind_failure(&self, line: &str, port: u16) -> bool {
        line.contains("bind() to ")
            && line.contains(&format!(":{port} failed"))
            && (line.contains("in use") || line.contains("(98:") || line.contains("(48:"))
    }

    fn holds(&self, holder: Option<&port::Holder>, port: u16, cfg: Option<&FrontConfig>) -> bool {
        match holder {
            Some(h) => h.name == "nginx",
            None => self.is_running() && cfg.is_none_or(|c| c.listens_on(port)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use proxlet_core::command::CommandOutput;

    use super::*;

    #[derive(Default)]
    struct Fake {
        answers: HashMap<String, CommandOutput>,
        calls: Mutex<Vec<String>>,
    }

    impl CommandRunner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            let line = format!("{program} {}", args.join(" "));
            self.calls.lock().unwrap().push(line.clone());
            self.answers
                .get(&line)
                .cloned()
                .ok_or_else(|| std::io::ErrorKind::NotFound.into())
        }
    }

    const DEBIAN_V: &str = "nginx version: nginx/1.24.0 (Ubuntu)\nbuilt with OpenSSL 3.0.13\nconfigure arguments: --with-cc-opt='-g' --prefix=/usr/share/nginx --conf-path=/etc/nginx/nginx.conf --http-log-path=/var/log/nginx/access.log --error-log-path=stderr --lock-path=/var/lock/nginx.lock\n";
    const BREW_V: &str = "nginx version: nginx/1.27.3\nconfigure arguments: --prefix=/opt/homebrew/Cellar/nginx/1.27.3 --sbin-path=/opt/homebrew/Cellar/nginx/1.27.3/bin/nginx --conf-path=/opt/homebrew/etc/nginx/nginx.conf --error-log-path=/opt/homebrew/var/log/nginx/error.log\n";

    #[test]
    fn parses_version_info() {
        let i = parse_version_info(Path::new("/usr/sbin/nginx"), DEBIAN_V).unwrap();
        assert_eq!(i.version, "1.24.0");
        assert_eq!(i.conf_path, Path::new("/etc/nginx/nginx.conf"));
        assert_eq!(i.prefix, Path::new("/usr/share/nginx"));
        let b = parse_version_info(Path::new("nginx"), BREW_V).unwrap();
        assert_eq!(b.conf_path, Path::new("/opt/homebrew/etc/nginx/nginx.conf"));
        assert_eq!(
            b.default_error_log.unwrap(),
            Path::new("/opt/homebrew/var/log/nginx/error.log")
        );
        let bare = parse_version_info(Path::new("nginx"), "nginx version: nginx/1.25.0\n").unwrap();
        assert_eq!(
            bare.conf_path,
            Path::new("/usr/local/nginx/conf/nginx.conf")
        );
        assert_eq!(
            bare.default_error_log.unwrap(),
            Path::new("/usr/local/nginx/logs/error.log")
        );
        assert!(parse_version_info(Path::new("nginx"), "not nginx").is_none());
    }

    #[test]
    fn detect_tries_candidates_in_order() {
        let mut fake = Fake::default();
        fake.answers.insert(
            "/usr/sbin/nginx -V".into(),
            CommandOutput {
                code: 0,
                stdout: String::new(),
                stderr: DEBIAN_V.into(),
            },
        );
        let n = Nginx::with_candidates(fake, &["nginx", "/usr/sbin/nginx"]);
        let inst = n.detect().unwrap();
        assert_eq!(inst.binary, Path::new("/usr/sbin/nginx"));
        assert_eq!(
            *n.runner.calls.lock().unwrap(),
            ["nginx -V", "/usr/sbin/nginx -V"]
        );
        assert!(
            Nginx::with_candidates(Fake::default(), &["nginx"])
                .detect()
                .is_none()
        );
    }

    fn inst() -> Installation {
        parse_version_info(Path::new("/usr/sbin/nginx"), DEBIAN_V).unwrap()
    }

    #[test]
    fn check_and_reload_report_nginx_output() {
        let mut fake = Fake::default();
        fake.answers.insert(
            "/usr/sbin/nginx -t -q -c /etc/nginx/nginx.conf".into(),
            CommandOutput {
                code: 1,
                stdout: String::new(),
                stderr:
                    "nginx: [emerg] unknown directive \"bogus\" in /etc/nginx/conf.d/x.conf:3\n"
                        .into(),
            },
        );
        fake.answers.insert(
            "/usr/sbin/nginx -s reload -c /etc/nginx/nginx.conf".into(),
            CommandOutput::default(),
        );
        let n = Nginx::new(fake);
        let err = n.check(&inst()).unwrap_err().to_string();
        assert!(err.contains("unknown directive \"bogus\""), "{err}");
        n.reload(&inst()).unwrap();
    }

    #[test]
    fn error_log_location() {
        let n = Nginx::new(Fake::default());
        let mut cfg = FrontConfig::default();
        let mut i = inst();
        i.default_error_log = Some("/var/log/nginx/error.log".into());
        assert_eq!(
            n.error_log(&i, &cfg),
            Some("/var/log/nginx/error.log".into())
        );
        cfg.error_log = Some("/var/log/custom.log".into());
        assert_eq!(n.error_log(&i, &cfg), Some("/var/log/custom.log".into()));
        cfg.error_log = Some("logs/err.log".into());
        assert_eq!(
            n.error_log(&i, &cfg),
            Some("/usr/share/nginx/logs/err.log".into())
        );
        cfg.error_log = Some("stderr".into());
        assert_eq!(n.error_log(&i, &cfg), None);
    }

    #[test]
    fn bind_failures_on_linux_and_macos() {
        let n = Nginx::new(Fake::default());
        let linux = "2026/11/23 10:00:00 [emerg] 1234#1234: bind() to 0.0.0.0:80 failed (98: Address already in use)";
        let v6 = "nginx: [emerg] bind() to [::]:80 failed (98: Address already in use)";
        let mac = "2026/11/23 10:00:00 [emerg] 77#0: bind() to 0.0.0.0:80 failed (48: Address already in use)";
        assert!(n.is_bind_failure(linux, 80));
        assert!(n.is_bind_failure(v6, 80));
        assert!(n.is_bind_failure(mac, 80));
        assert!(!n.is_bind_failure(linux, 8080));
        assert!(!n.is_bind_failure(
            "bind() to 0.0.0.0:8080 failed (98: Address already in use)",
            80
        ));
        assert!(!n.is_bind_failure("bind() to 0.0.0.0:80 failed (13: Permission denied)", 80));
    }

    #[test]
    fn holder_recognition() {
        let n = Nginx::new(Fake::default());
        let nginx = port::Holder {
            pid: 1,
            name: "nginx".into(),
        };
        let caddy = port::Holder {
            pid: 2,
            name: "caddy".into(),
        };
        assert!(n.holds(Some(&nginx), 80, None));
        assert!(!n.holds(Some(&caddy), 80, None));
    }

    #[test]
    fn removes_the_flag_only() {
        assert_eq!(
            without_flag("    listen 80 default_server;", "default_server").unwrap(),
            "    listen 80;"
        );
        assert_eq!(
            without_flag(
                "\tlisten [::]:80 default_server ipv6only=on;",
                "default_server"
            )
            .unwrap(),
            "\tlisten [::]:80 ipv6only=on;"
        );
        assert_eq!(
            without_flag("listen 80 default; # default_server once", "default").unwrap(),
            "listen 80; # default_server once"
        );
        assert!(without_flag("listen 80;", "default_server").is_none());
        assert!(without_flag("listen 80 not_default_server;", "default_server").is_none());
    }

    #[test]
    fn rendered_block_is_marked_and_points_at_the_internal_listener() {
        let r = render(80, "127.0.0.1:47080".parse().unwrap(), true);
        assert!(r.starts_with(MANAGED_MARKER));
        assert!(r.contains("listen 80 default_server;"));
        assert!(r.contains("listen [::]:80 default_server;"));
        assert!(r.contains("proxy_pass http://127.0.0.1:47080;"));
        assert!(r.contains("proxy_buffering off;"));
        assert!(!render(80, "127.0.0.1:47080".parse().unwrap(), false).contains("[::]"));
    }
}
