//! Live statistics over the control socket, and `proxlet monitor`.
#![allow(missing_docs)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use proxlet::control::{Request, StatsReply, StatsRow};

fn stats(home: &Home) -> StatsReply {
    let reply = home.control(&Request::Stats);
    assert!(reply.ok, "{reply:?}");
    reply.stats.expect("stats in the reply")
}

fn row(home: &Home, rule: Option<&str>) -> StatsRow {
    stats(home)
        .rows
        .into_iter()
        .find(|r| r.rule.as_deref() == rule)
        .unwrap_or_else(|| panic!("no row {rule:?}"))
}

/// Waits until the row's total is `n`.
fn wait_total(home: &Home, rule: Option<&str>, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let r = row(home, rule);
        if r.total == n {
            return;
        }
        assert!(Instant::now() < deadline, "{r:#?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn snapshot_over_the_control_socket() {
    let home = Home::new();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    home.run(&["add", "/", &free_port().to_string()]).ok();
    let _daemon = home.start();
    assert_eq!(home.get("x", "/api/users").0, 502);
    wait_total(&home, Some("/api"), 1);
    let s = stats(&home);
    let rules: Vec<_> = s.rows.iter().map(|r| r.rule.clone()).collect();
    assert_eq!(rules, [Some("/".into()), Some("/api".into()), None]);
    let api = &s.rows[1];
    assert_eq!((api.status_5xx, api.upstream_errors), (1, 1));
    assert!(api.p50_ms.is_some());
    assert!(api.target.as_deref().unwrap().starts_with("127.0.0.1:"));
}

#[test]
fn rows_follow_rule_changes() {
    let home = Home::new();
    let site = tempfile::tempdir().unwrap();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    let _daemon = home.start();

    // New rule: a row with no traffic.
    home.run(&["add", "/docs", site.path().to_str().unwrap()])
        .ok();
    assert_eq!(row(&home, Some("/docs")).total, 0);

    // Target changed: totals kept, new target shown.
    home.get("x", "/api");
    wait_total(&home, Some("/api"), 1);
    let port = free_port().to_string();
    home.run(&["add", "/api", &port]).ok();
    let api = row(&home, Some("/api"));
    assert_eq!(api.total, 1);
    assert_eq!(api.target, Some(format!("127.0.0.1:{port}")));

    // Removed and added again: from zero.
    home.run(&["rm", "/api"]).ok();
    assert!(
        stats(&home)
            .rows
            .iter()
            .all(|r| r.rule.as_deref() != Some("/api"))
    );
    home.run(&["add", "/api", &port]).ok();
    assert_eq!(row(&home, Some("/api")).total, 0);
}

#[test]
fn totals_start_from_zero_after_a_restart() {
    let home = Home::new();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    let daemon = home.start();
    home.get("x", "/api");
    home.get("x", "/api/again");
    wait_total(&home, Some("/api"), 2);
    daemon.stop();
    let _daemon = home.start();
    assert_eq!(row(&home, Some("/api")).total, 0);
}

#[test]
fn piped_prints_one_plain_snapshot() {
    let home = Home::new();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    home.run(&["add", "/", &free_port().to_string()]).ok();
    let _daemon = home.start();
    home.get("x", "/api");
    wait_total(&home, Some("/api"), 1);
    // Tests run with stdout piped, as in `proxlet monitor | cat`.
    let out = home.run(&["monitor"]);
    out.ok();
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert!(lines[0].starts_with("RULE "), "{}", out.stdout);
    let firsts: Vec<&str> = lines[1..]
        .iter()
        .map(|l| l.split("  ").next().unwrap().trim())
        .collect();
    assert_eq!(firsts, ["/", "/api", "(no rule)"], "{}", out.stdout);
    assert!(!out.stdout.contains('\x1b'), "no escape sequences");
    // The /api row: total 1, one 5xx, one upstream error.
    let api: Vec<&str> = lines[2].split_whitespace().collect();
    assert_eq!(&api[3..7], ["1", "0", "1", "1"], "{}", lines[2]);
}

#[test]
fn once_prints_the_same_table() {
    let home = Home::new();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    let _daemon = home.start();
    let out = home.run(&["monitor", "--once"]);
    out.ok();
    assert_eq!(out.stdout.lines().count(), 3, "{}", out.stdout);
    assert!(out.stdout.lines().nth(1).unwrap().starts_with("/api "));
}

#[test]
fn not_running_says_how_to_start() {
    let home = Home::new();
    let out = home.run(&["monitor"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("proxlet is not running"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("proxlet start"), "{}", out.stderr);
    assert_eq!(out.stdout, "");
}

#[test]
fn monitor_flags_complete() {
    let home = Home::new();
    assert!(home.complete(&[""]).contains(&"monitor".to_owned()));
    // `Home::complete` hides flags; ask for them directly.
    let out = home
        .command(&["--", "proxlet", "monitor", "--"])
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .output()
        .unwrap();
    let offered = String::from_utf8_lossy(&out.stdout);
    assert!(
        offered.lines().any(|l| l.starts_with("--once")),
        "{offered}"
    );
}

/// Runs `proxlet monitor` on a pseudo-terminal: the live table appears, `q`
/// quits with status 0, and the terminal is restored.
#[test]
fn live_table_in_a_terminal() {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};

    let home = Home::new();
    home.run(&["add", "/api", &free_port().to_string()]).ok();
    let _daemon = home.start();
    home.get("x", "/api");
    wait_total(&home, Some("/api"), 1);

    let size = nix::pty::Winsize {
        ws_row: 20,
        ws_col: 140,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = nix::pty::openpty(Some(&size), None).unwrap();
    let slave = std::fs::File::from(pty.slave);
    let mut child = home
        .command(&["monitor"])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let mut master = std::fs::File::from(pty.master);
    let screen = Arc::new(Mutex::new(Vec::new()));
    {
        let mut reader = master.try_clone().unwrap();
        let screen = screen.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                screen.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }
    let seen = |needle: &str| String::from_utf8_lossy(&screen.lock().unwrap()).contains(needle);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(seen("proxlet monitor") && seen("/api") && seen("(no rule)")) {
        assert!(
            Instant::now() < deadline,
            "no table: {:?}",
            String::from_utf8_lossy(&screen.lock().unwrap())
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    master.write_all(b"q").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "q did not quit");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
    let out = screen.lock().unwrap().clone();
    let has = |seq: &[u8]| out.windows(seq.len()).any(|w| w == seq);
    assert!(has(b"\x1b[?1049h"), "entered the alternate screen");
    assert!(has(b"\x1b[?1049l"), "left the alternate screen");
}
