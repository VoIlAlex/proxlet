//! Log files written by `proxlet run`, and `proxlet logs`.
#![allow(missing_docs)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::*;
use serde_json::Value;

fn log_dir(home: &Home) -> PathBuf {
    home.path().join("state/logs")
}

/// Every stored record, oldest first.
fn records(home: &Home) -> Vec<Value> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(log_dir(home))
        .map(|d| d.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default();
    files.sort_by_key(|p| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        // proxlet.log is the newest.
        (name == "proxlet.log", name)
    });
    files
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str::<Value>(l).expect(l))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Waits until a stored record satisfies `pred`.
fn wait_for_record(home: &Home, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let all = records(home);
        if let Some(r) = all.iter().find(|r| pred(r)) {
            return r.clone();
        }
        assert!(Instant::now() < deadline, "no {what} record in {all:#?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn is_event_containing(r: &Value, text: &str) -> bool {
    r["kind"] == "event" && r["msg"].as_str().is_some_and(|m| m.contains(text))
}

#[test]
fn requests_and_events_reach_the_log_files() {
    let home = Home::new();
    let daemon = home.start();
    assert_eq!(home.get("Example.com", "/nope?secret=1").0, 404);
    let r = wait_for_record(&home, "request", |r| r["kind"] == "request");
    assert_eq!(r["method"], "GET");
    assert_eq!(r["host"], "example.com");
    assert_eq!(r["path"], "/nope");
    assert_eq!(r["status"], 404);
    assert_eq!(r["client"], "127.0.0.1");
    assert!(r.get("rule").is_none());
    assert!(!r.to_string().contains("secret"));
    let started = wait_for_record(&home, "start event", |r| {
        is_event_containing(r, "started with")
    });
    assert_eq!(started["level"], "info");
    // stderr is unchanged.
    daemon.wait_for_log("listening on http://", Duration::from_secs(5));

    let mode = std::fs::metadata(log_dir(&home))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn port_taken_is_an_error_event() {
    let home = Home::new();
    let _holder = std::net::TcpListener::bind(("127.0.0.1", home.port)).unwrap();
    let _daemon = home.start();
    let r = wait_for_record(&home, "port taken", |r| {
        is_event_containing(r, &format!("port {} is already in use by", home.port))
    });
    assert_eq!(r["level"], "error");
    assert!(
        r["msg"]
            .as_str()
            .unwrap()
            .contains(&format!("(pid {})", std::process::id())),
        "{r}"
    );
}

#[test]
fn records_are_written_before_the_server_exits() {
    let home = Home::new();
    let daemon = home.start();
    for _ in 0..20 {
        home.get("x", "/");
    }
    assert_eq!(daemon.stop(), 0);
    let all = records(&home);
    assert_eq!(all.iter().filter(|r| r["kind"] == "request").count(), 20);
    assert!(
        all.last()
            .is_some_and(|r| is_event_containing(r, "stopped"))
    );
}

#[test]
fn access_logging_off_keeps_events() {
    let home = Home::new();
    let _daemon = home.start();
    home.run(&["config", "log.access", "off"]).ok();
    home.get("x", "/");
    wait_for_record(&home, "config event", |r| r["kind"] == "event");
    std::thread::sleep(Duration::from_millis(500));
    assert!(records(&home).iter().all(|r| r["kind"] == "event"));
}

/// Rules `/api` → a closed port, `example.com/` → another closed port and
/// `/files` → a directory, with one request to each.
fn three_rules(home: &Home) -> (u16, u16) {
    let (api, site) = (free_port(), free_port());
    std::fs::create_dir_all(home.path().join("site")).unwrap();
    home.run(&["add", "/api", &api.to_string(), "--yes"]).ok();
    home.run(&["add", "example.com/", &site.to_string(), "--yes"])
        .ok();
    home.run(&["add", "/files", "site"]).ok();
    (api, site)
}

fn request_each(home: &Home) {
    assert_eq!(home.get("x", "/api/users").0, 502);
    assert_eq!(home.get("example.com", "/api").0, 502);
    assert_eq!(home.get("example.com", "/").0, 502);
    assert_eq!(home.get("x", "/files/").0, 200);
}

fn json_lines(out: &str) -> Vec<Value> {
    out.lines()
        .map(|l| serde_json::from_str(l).expect(l))
        .collect()
}

#[test]
fn all_history_selectors_and_reading_while_stopped() {
    let home = Home::new();
    let (api, site) = three_rules(&home);
    let daemon = home.start();
    request_each(&home);
    wait_for_record(&home, "last request", |r| r["path"] == "/files/");
    assert_eq!(daemon.stop(), 0);

    // All history, oldest first, events included, while stopped.
    let all = json_lines(&home.run(&["logs", "--all", "--json"]).ok().stdout);
    let paths: Vec<&str> = all.iter().filter_map(|r| r["path"].as_str()).collect();
    assert_eq!(paths, ["/api/users", "/api", "/", "/files/"]);
    assert!(all.first().is_some_and(|r| r["kind"] == "event"));
    assert!(
        all.last()
            .is_some_and(|r| is_event_containing(r, "stopped"))
    );

    let select = |sel: &str| -> Vec<String> {
        json_lines(&home.run(&["logs", sel, "--all", "--json"]).ok().stdout)
            .iter()
            .map(|r| {
                assert_eq!(r["kind"], "request");
                format!(
                    "{}{}",
                    r["host"].as_str().unwrap(),
                    r["path"].as_str().unwrap()
                )
            })
            .collect()
    };
    // By route: the rule, exactly. The host rule answered `example.com/api`.
    assert_eq!(select("/api"), ["x/api/users"]);
    assert_eq!(select("example.com/"), ["example.com/api", "example.com/"]);
    assert_eq!(select("/files"), ["x/files/"]);
    // By domain, whatever the rule.
    assert_eq!(select("example.com"), ["example.com/api", "example.com/"]);
    // By upstream port.
    assert_eq!(select(&api.to_string()), ["x/api/users"]);
    assert_eq!(
        select(&site.to_string()),
        ["example.com/api", "example.com/"]
    );
    assert_eq!(select("1"), Vec::<String>::new());
}

#[test]
fn piped_and_json_output() {
    let home = Home::new();
    three_rules(&home);
    let _daemon = home.start();
    request_each(&home);
    wait_for_record(&home, "last request", |r| r["path"] == "/files/");

    // Piped, with colours allowed: plain aligned lines.
    let out = home
        .command(&["logs", "--all"])
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains('\x1b'), "{text:?}");
    let failed: Vec<&str> = text.lines().filter(|l| l.contains(" 502 ")).collect();
    assert_eq!(failed.len(), 3, "{text}");
    assert!(
        failed[0].contains("x/api/users") && failed[0].contains("connection refused"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains(" 200 ") && l.contains("→ "))
    );
    assert!(
        text.lines()
            .any(|l| l.contains("INFO") && l.contains("started with"))
    );

    // JSON: the stored lines, byte for byte.
    let json = home.run(&["logs", "--all", "--json"]).ok().stdout.clone();
    let stored = std::fs::read_to_string(log_dir(&home).join("proxlet.log")).unwrap();
    assert!(stored.starts_with(&json), "{json}\n---\n{stored}");
}

#[test]
fn invalid_selector_and_no_records() {
    let home = Home::new();
    let out = home.run(&["logs", "bad host", "--all"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("is not a route, a port or a domain name"),
        "{}",
        out.stderr
    );
    let out = home.run(&["logs", "70000", "--all"]);
    assert_eq!(out.code, 1);

    let out = home.run(&["logs", "--all"]);
    out.ok();
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("No log records yet"), "{}", out.stderr);
}

/// `proxlet logs <args>` running in the background, its stdout lines
/// collected as they arrive.
struct Following {
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
}

impl Following {
    fn start(home: &Home, args: &[&str]) -> Self {
        let mut full = vec!["logs"];
        full.extend_from_slice(args);
        let mut child = home
            .command(&full)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Self { child, lines }
    }

    /// The next line, if one arrives within `within`.
    fn next(&self, within: Duration) -> Option<String> {
        self.lines.recv_timeout(within).ok()
    }

    /// Sends Ctrl-C and returns the exit code.
    fn interrupt(mut self) -> i32 {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.child.id()).unwrap());
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGINT).unwrap();
        self.child.wait().unwrap().code().unwrap_or(-1)
    }
}

#[test]
fn follow_shows_the_last_records_then_new_ones() {
    let home = Home::new();
    let _daemon = home.start();
    for i in 0..25 {
        home.get("x", &format!("/old{i}"));
    }
    wait_for_record(&home, "last old request", |r| r["path"] == "/old24");

    let f = Following::start(&home, &["--json"]);
    let mut history = Vec::new();
    while let Some(line) = f.next(Duration::from_secs(2)) {
        history.push(serde_json::from_str::<Value>(&line).unwrap());
        if history.len() == 20 {
            break;
        }
    }
    assert_eq!(history.len(), 20);
    assert_eq!(history.last().unwrap()["path"], "/old24");
    assert_eq!(history.first().unwrap()["path"], "/old5");

    let asked = Instant::now();
    home.get("x", "/new");
    let line = f.next(Duration::from_secs(5)).expect("the new request");
    assert!(
        asked.elapsed() < Duration::from_millis(1500),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["path"],
        "/new"
    );
    assert_eq!(f.interrupt(), 0);
}

#[test]
fn follow_before_any_record_and_with_a_selector() {
    let home = Home::new();
    let f = Following::start(&home, &["/api", "--json"]);
    std::fs::create_dir_all(home.path().join("site")).unwrap();
    home.run(&["add", "/api", "site"]).ok();
    let _daemon = home.start();
    home.get("x", "/other");
    home.get("x", "/api/");
    let line = f.next(Duration::from_secs(5)).expect("the /api request");
    let r: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(r["path"], "/api/");
    assert!(f.next(Duration::from_millis(500)).is_none());
    assert_eq!(f.interrupt(), 0);
}

#[test]
fn rotation_while_following() {
    let mut home = Home::new();
    home.set_env("PROXLET_TEST_LOG_FILE_BYTES", "2000");
    let _daemon = home.start();
    let f = Following::start(&home, &["--json"]);
    std::thread::sleep(Duration::from_millis(300));
    let n = 60;
    for i in 0..n {
        home.get("x", &format!("/r{i}"));
    }
    let mut seen = Vec::new();
    while seen.len() < n {
        let Some(line) = f.next(Duration::from_secs(5)) else {
            break;
        };
        let r: Value = serde_json::from_str(&line).unwrap();
        if r["kind"] == "request" {
            seen.push(r["path"].as_str().unwrap().to_owned());
        }
    }
    let expected: Vec<String> = (0..n).map(|i| format!("/r{i}")).collect();
    assert_eq!(seen, expected);
    while let Some(line) = f.next(Duration::from_millis(500)) {
        assert!(!line.contains(r#""kind":"request""#), "repeated: {line}");
    }
    let rotated = std::fs::read_dir(log_dir(&home))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("proxlet-")
        })
        .count();
    assert!(rotated >= 3, "only {rotated} rotations");
    assert_eq!(f.interrupt(), 0);
}

#[test]
fn complete_a_selector() {
    let home = Home::new();
    home.run(&["add", "/api", "8000", "--yes"]).ok();
    home.run(&["add", "example.com/", "3000", "--yes"]).ok();
    assert_eq!(
        home.complete(&["logs", ""]),
        ["/api", "example.com/", "example.com", "8000", "3000"]
    );
}

#[test]
fn unwritable_log_directory_still_serves_and_warns() {
    let home = Home::new();
    // A file where the directory should be: it can never be created.
    std::fs::create_dir_all(home.path().join("state")).unwrap();
    std::fs::write(log_dir(&home), "not a directory").unwrap();
    let daemon = home.start();
    for _ in 0..5 {
        assert_eq!(home.get("x", "/").0, 404);
    }
    let log = daemon.wait_for_log("cannot write the log in", Duration::from_secs(5));
    assert_eq!(log.matches("cannot write the log in").count(), 1, "{log}");
    assert_eq!(home.run(&["status"]).code, 0);
}

#[test]
fn configuration_changes_are_events() {
    let home = Home::new();
    let _daemon = home.start();
    let port = free_port().to_string();
    home.run(&["add", "/api", &port]).ok();
    home.run(&["add", "/api", "9"]).ok();
    home.run(&["rm", "/api"]).ok();
    home.run(&["config", "log.max_age", "3d"]).ok();
    for text in [
        format!("rule added: /api → http://127.0.0.1:{port}"),
        format!("rule updated: /api → http://127.0.0.1:9 (was http://127.0.0.1:{port})"),
        "rule removed: /api (was → http://127.0.0.1:9)".to_owned(),
        "setting changed: log.max_age 3d (was 7d)".to_owned(),
    ] {
        wait_for_record(&home, &text, |r| is_event_containing(r, &text));
    }
    let shown = home.run(&["logs", "--all"]).ok().stdout.clone();
    assert!(shown.contains("rule added: /api"), "{shown}");
}
