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
