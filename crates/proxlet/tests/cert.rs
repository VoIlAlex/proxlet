//! `proxlet cert` from the command line, against the fake CA test hook and a
//! fake resolver (`PROXLET_TEST_RESOLVE`).
#![allow(missing_docs)]

mod common;

use std::io::{Read, Write};
use std::time::Duration;

use common::*;

/// A home where `names` resolve to this proxlet.
fn home_resolving(names: &[&str]) -> Home {
    let mut home = Home::new();
    let map: Vec<String> = names.iter().map(|n| format!("{n}=127.0.0.1")).collect();
    home.set_env("PROXLET_TEST_RESOLVE", &map.join(";"));
    home
}

fn cert(home: &Home, args: &[&str]) -> Run {
    let mut all = vec!["cert"];
    all.extend_from_slice(args);
    home.run(&all)
}

#[test]
fn certificate_issued_listed_and_served() {
    let home = home_resolving(&["app.example.com"]);
    let _daemon = home.start();
    let out = cert(&home, &["app.example.com", "--yes"]);
    out.ok();
    let last = out.stdout.lines().last().unwrap_or_default();
    assert!(
        last.starts_with("✓ Certificate for app.example.com, valid until 20"),
        "{}{}",
        out.stdout,
        out.stderr
    );
    let ls = cert(&home, &["ls"]).ok().stdout.clone();
    let fields: Vec<&str> = ls.trim_end_matches('\n').split('\t').collect();
    assert_eq!(fields.len(), 4, "{ls:?}");
    assert_eq!(fields[0], "app.example.com");
    assert!(fields[2] == "89" || fields[2] == "90", "{ls:?}");
    assert_eq!(fields[3], "");
    // A bare `proxlet cert` lists too.
    assert_eq!(cert(&home, &[]).ok().stdout, ls);
    assert_eq!(home.https_get("app.example.com", "/").unwrap().0, 404);
}

#[test]
fn several_domains_and_one_fails() {
    let home = home_resolving(&["b.example.com"]);
    let _daemon = home.start();
    let out = cert(&home, &["a.example.com", "b.example.com", "--yes"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("a.example.com does not resolve")
            && out.stderr.contains("A or AAAA record"),
        "{}",
        out.stderr
    );
    assert!(
        out.stdout.contains("✓ Certificate for b.example.com"),
        "{}",
        out.stdout
    );
    assert!(cert(&home, &["ls"]).stdout.starts_with("b.example.com\t"));
}

#[test]
fn already_has_one() {
    let home = home_resolving(&["app.example.com"]);
    let _daemon = home.start();
    cert(&home, &["app.example.com", "--yes"]).ok();
    let out = cert(&home, &["app.example.com"]);
    out.ok();
    assert!(
        out.stdout
            .contains("app.example.com already has a certificate, valid until")
            && out.stderr.contains("proxlet cert renew app.example.com"),
        "{}{}",
        out.stdout,
        out.stderr
    );
    assert!(!out.stderr.contains("Requesting"), "{}", out.stderr);
}

#[test]
fn server_not_running() {
    let home = home_resolving(&["app.example.com"]);
    let out = cert(&home, &["app.example.com", "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("proxlet must be running to answer Let's Encrypt's challenge")
            && out.stderr.contains("proxlet start")
            && out.stderr.contains("proxlet run"),
        "{}",
        out.stderr
    );
}

#[test]
fn local_names_are_refused_without_contacting_the_ca() {
    let home = home_resolving(&[]);
    let _daemon = home.start();
    let out = cert(&home, &["myapp.test", "--yes"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("Let's Encrypt cannot issue for `.test` names because they are not public"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("Requesting"), "{}", out.stderr);
}

#[test]
fn name_answered_by_another_server() {
    // Some other web server answers for the name.
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = other.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in other.incoming().flatten() {
            // Read the whole request so closing does not reset the connection.
            let mut req = Vec::new();
            let mut byte = [0u8; 1];
            while !req.ends_with(b"\r\n\r\n") && s.read(&mut byte).unwrap_or(0) == 1 {
                req.push(byte[0]);
            }
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            );
        }
    });
    let mut home = Home::new();
    home.set_env(
        "PROXLET_TEST_RESOLVE",
        &format!("app.example.com=127.0.0.1:{port}"),
    );
    let _daemon = home.start();
    let out = cert(&home, &["app.example.com", "--yes"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains(
            "app.example.com reaches a server other than this proxlet (it resolves to 127.0.0.1)"
        ),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("Requesting"), "{}", out.stderr);
}

#[test]
fn probe_cannot_be_sent_warns_and_continues() {
    let mut home = Home::new();
    home.set_env(
        "PROXLET_TEST_RESOLVE",
        &format!("app.example.com=127.0.0.1:{}", free_port()),
    );
    let _daemon = home.start();
    let out = cert(&home, &["app.example.com", "--yes"]);
    out.ok();
    assert!(
        out.stderr
            .contains("could not check from here that app.example.com reaches proxlet"),
        "{}",
        out.stderr
    );
    assert!(
        out.stdout.contains("✓ Certificate for app.example.com"),
        "{}",
        out.stdout
    );
}

#[test]
fn terms_without_a_terminal() {
    let home = home_resolving(&["app.example.com"]);
    let _daemon = home.start();
    let out = cert(&home, &["app.example.com"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("https://fake-acme.invalid/terms") && out.stderr.contains("--yes"),
        "{}",
        out.stderr
    );
    assert!(cert(&home, &["ls"]).stderr.contains("No certificates yet"));
}

#[test]
fn none_yet() {
    let home = Home::new();
    let out = cert(&home, &["ls"]);
    out.ok();
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr.contains("No certificates yet")
            && out.stderr.contains("proxlet cert example.com"),
        "{}",
        out.stderr
    );
}

#[test]
fn remove_and_renew() {
    let home = home_resolving(&["app.example.com", "b.example.com"]);
    let _daemon = home.start();
    cert(&home, &["app.example.com", "b.example.com", "--yes"]).ok();

    std::thread::sleep(Duration::from_millis(1100));
    let out = cert(&home, &["renew", "app.example.com"]);
    out.ok();
    assert!(
        out.stdout
            .contains("Renewed the certificate for app.example.com, valid until"),
        "{}",
        out.stdout
    );

    let out = cert(&home, &["rm", "nope.example.com"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("there is no certificate for nope.example.com")
            && out
                .stderr
                .contains("Certificates exist for: app.example.com, b.example.com"),
        "{}",
        out.stderr
    );

    cert(&home, &["rm", "app.example.com"]).ok();
    let ls = cert(&home, &["ls"]).ok().stdout.clone();
    assert!(
        !ls.contains("app.example.com") && ls.contains("b.example.com"),
        "{ls}"
    );
    assert!(
        home.https_get("app.example.com", "/").is_err(),
        "handshake fails"
    );
    assert_eq!(home.get("app.example.com", "/").0, 404, "not redirected");
}

#[test]
fn staging_is_noted() {
    let home = home_resolving(&["app.example.com"]);
    home.run(&["config", "cert.server", "staging"]).ok();
    let _daemon = home.start();
    cert(&home, &["app.example.com", "--yes"]).ok();
    let ls = cert(&home, &["ls"]).ok().stdout.clone();
    assert!(ls.trim_end().ends_with("\tstaging"), "{ls:?}");
}

#[test]
fn completion_offers_rule_hosts_and_stored_domains() {
    let home = home_resolving(&["a.example.com", "b.example.com"]);
    home.run(&["add", "site.example.org", "/tmp"]).ok();
    home.run(&["add", "/api", "8000"]).ok();
    let offered = home.complete(&["cert", ""]);
    assert!(
        offered.contains(&"site.example.org".to_owned()),
        "{offered:?}"
    );
    assert!(!offered.iter().any(|o| o.starts_with('/')), "{offered:?}");
    let _daemon = home.start();
    cert(&home, &["a.example.com", "b.example.com", "--yes"]).ok();
    assert_eq!(
        home.complete(&["cert", "rm", ""]),
        ["a.example.com", "b.example.com"]
    );
    assert_eq!(home.complete(&["cert", "renew", "b"]), ["b.example.com"]);
}
