//! `proxlet run`, the port check, the control channel and the apply pipeline.
#![allow(missing_docs)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use common::*;

#[test]
fn run_serves_rules_and_stops_cleanly_on_sigterm() {
    let home = Home::new();
    let site = home.path().join("site");
    std::fs::create_dir(&site).unwrap();
    std::fs::write(site.join("hello.txt"), "hi").unwrap();
    home.run(&["add", "/files", "site"]).ok();

    let daemon = home.start();
    assert_eq!(home.get("x", "/files/hello.txt"), (200, "hi".into()));
    assert_eq!(home.get("x", "/nope").0, 404);
    assert_eq!(daemon.stop(), 0);
    assert!(!home.socket().exists(), "socket left behind");
    assert_ne!(home.run(&["status"]).code, 0);
}

#[test]
fn ctrl_c_shuts_down_cleanly() {
    let home = Home::new();
    let daemon = home.start();
    assert_eq!(home.get("x", "/").0, 404);
    assert_eq!(daemon.interrupt(), 0);
    assert!(!home.socket().exists());
}

#[test]
fn port_taken_waits_and_names_the_holder() {
    let home = Home::new();
    let holder = std::net::TcpListener::bind(("127.0.0.1", home.port)).unwrap();
    let daemon = home.start();
    // The holder is this test process; lsof or /proc should find it.
    let log = daemon.wait_for_log("waiting for it to be free", Duration::from_secs(10));
    assert!(
        log.contains(&format!("port {} is already in use by", home.port)),
        "{log}"
    );
    assert!(
        log.contains(&format!("(pid {})", std::process::id())),
        "holder not named: {log}"
    );
    home.wait_for_status(
        &format!("waiting for port {} (held by ", home.port),
        Duration::from_secs(5),
    );
    // Rules are still served on the internal listener meanwhile.
    assert_eq!(home.get_internal("x", "/").0, 404);

    // Port freed while waiting: proxlet takes it within 5 seconds.
    drop(holder);
    home.wait_for_status("mode    standalone", Duration::from_secs(5));
    assert_eq!(home.get("x", "/").0, 404);
}

#[test]
fn second_run_says_already_running() {
    let home = Home::new();
    let _daemon = home.start();
    let out = home.run(&["run"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("already running"), "{}", out.stderr);
    // The first one is unaffected.
    assert_eq!(home.run(&["status"]).code, 0);
}

#[test]
fn status_reports_running_details() {
    let home = Home::new();
    let daemon = home.start();
    let out = home.run(&["status"]);
    out.ok();
    for needle in [
        "running".to_owned(),
        "mode    standalone".to_owned(),
        format!("http://127.0.0.1:{}", home.port),
        format!("foreground (pid {})", daemon.pid()),
        "rules   0".to_owned(),
    ] {
        assert!(
            out.stdout.contains(&needle),
            "missing {needle:?}:\n{}",
            out.stdout
        );
    }
}

#[test]
fn status_when_stopped_exits_nonzero_with_hint() {
    let home = Home::new();
    let out = home.run(&["status"]);
    assert_eq!(out.code, 3);
    assert!(out.stdout.contains("not running"));
    assert!(out.stderr.contains("proxlet start"), "{}", out.stderr);
}

fn control(home: &Home, request: &str) -> String {
    let stream = UnixStream::connect(home.socket()).unwrap();
    let mut reader = BufReader::new(stream);
    writeln!(reader.get_mut(), "{request}").unwrap();
    let mut reply = String::new();
    reader.read_line(&mut reply).unwrap();
    reply
}

#[test]
fn rejected_changes_leave_the_file_byte_identical() {
    let home = Home::new();
    home.run(&["add", "/api", "8000"]).ok();
    let _daemon = home.start();
    let before = std::fs::read(home.config_file()).unwrap();

    let out = home.run(&["rm", "/nope"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("no rule for `/nope`"), "{}", out.stderr);

    // Bypass the CLI's own validation: the daemon must still refuse.
    let reply = control(
        &home,
        r#"{"cmd":"apply","change":{"kind":"add_rule","route":"/x","target":"/definitely/not/here"}}"#,
    );
    assert!(reply.contains(r#""ok":false"#), "{reply}");
    assert!(reply.contains("does not exist"), "{reply}");

    let reply = control(&home, "this is not json");
    assert!(reply.contains("malformed request"), "{reply}");

    assert_eq!(std::fs::read(home.config_file()).unwrap(), before);
}

#[test]
fn shutdown_request_stops_the_daemon() {
    let home = Home::new();
    let _daemon = home.start();
    let reply = control(&home, r#"{"cmd":"shutdown"}"#);
    assert!(reply.contains(r#""ok":true"#));
    let deadline = Instant::now() + Duration::from_secs(10);
    while home.run(&["status"]).code == 0 {
        assert!(Instant::now() < deadline, "daemon did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[test]
fn handoff_release_frees_the_port_and_reclaim_takes_it_back() {
    let home = Home::new();
    let _daemon = home.start();
    assert!(!port_is_free(home.port));
    assert!(!port_is_free(home.https_port));

    let out = home.run(&["handoff", "release"]);
    out.ok();
    assert!(
        out.stderr.contains("released the HTTP and HTTPS ports"),
        "{}",
        out.stderr
    );
    // The release returns only once both listeners are closed.
    assert!(
        port_is_free(home.port),
        "HTTP port still held after release"
    );
    assert!(
        port_is_free(home.https_port),
        "HTTPS port still held after release"
    );
    // The internal listener keeps serving meanwhile.
    assert_eq!(home.get_internal("x", "/").0, 404);

    home.run(&["handoff", "reclaim"]).ok();
    home.wait_for_status("mode    standalone", Duration::from_secs(5));
    assert_eq!(home.get("x", "/").0, 404);
    home.wait_for_status(
        &format!("https   https://127.0.0.1:{}", home.https_port),
        Duration::from_secs(5),
    );
}

#[test]
fn handoff_succeeds_when_proxlet_is_not_running() {
    let home = Home::new();
    for action in ["release", "reclaim"] {
        let out = home.run(&["handoff", action]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(out.stderr.contains("not running"), "{}", out.stderr);
    }
    // An explicit socket that does not exist is fine too.
    home.run(&[
        "handoff",
        "release",
        "--socket",
        "/nonexistent/proxlet.sock",
    ])
    .ok();
}
