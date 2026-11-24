//! The daemon's HTTPS port, issuing and renewing through the control socket
//! (with the fake CA test hook), the redirect, and the status lines.
#![allow(missing_docs)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use proxlet::control::Request;

fn issue(home: &Home, domain: &str) -> proxlet::control::Reply {
    home.control(&Request::Issue {
        domain: domain.into(),
        accept_terms: true,
    })
}

fn not_after(home: &Home, domain: &str) -> i64 {
    proxlet_cert::Store::new(home.path().join("state/certs"))
        .get(domain)
        .unwrap()
        .unwrap()
        .leaf
        .not_after
}

#[test]
fn https_port_taken_keeps_http_serving() {
    let home = Home::new();
    let _holder = std::net::TcpListener::bind(("127.0.0.1", home.https_port)).unwrap();
    let _daemon = home.start();
    let out = home.wait_for_status(
        &format!("https   waiting for port {} (held by", home.https_port),
        Duration::from_secs(10),
    );
    assert!(out.contains("mode    standalone"), "{out}");
    assert_eq!(home.get("x", "/").0, 404);
}

#[test]
fn terms_then_issue_then_https_and_redirect() {
    let home = Home::new();
    home.run(&["add", "app.example.com", "/tmp"]).ok();
    let _daemon = home.start();

    // No account yet: the terms come back first.
    let reply = home.control(&Request::Issue {
        domain: "app.example.com".into(),
        accept_terms: false,
    });
    assert!(!reply.ok);
    assert_eq!(
        reply.terms.as_deref(),
        Some("https://fake-acme.invalid/terms")
    );

    let reply = issue(&home, "app.example.com");
    assert!(reply.ok, "{reply:?}");
    let cert = reply.cert.unwrap();
    assert_eq!(cert.domain, "app.example.com");

    // Served over HTTPS right away, with the issued certificate.
    let (status, _) = home.https_get("app.example.com", "/").unwrap();
    assert_eq!(status, 200);
    assert!(
        home.https_get("other.example.com", "/").is_err(),
        "no certificate, no handshake"
    );

    // Plain HTTP for that host now redirects; other hosts do not.
    let (status, raw) = http_get_raw(home.port, "app.example.com", "/a?b=1");
    assert_eq!(status, 308, "{raw}");
    assert!(
        raw.contains(&format!(
            "location: https://app.example.com:{}/a?b=1",
            home.https_port
        )),
        "{raw}"
    );
    assert_eq!(home.get("other.example.com", "/").0, 404);

    // `tls.redirect off` applies live.
    home.run(&["config", "tls.redirect", "off"]).ok();
    assert_eq!(home.get("app.example.com", "/").0, 200);

    let out = home.run(&["status"]).stdout;
    assert!(
        out.contains(&format!(
            "https   https://127.0.0.1:{} (1 certificate)",
            home.https_port
        )),
        "{out}"
    );
}

#[test]
fn no_redirect_while_https_is_not_served() {
    let home = Home::new();
    let _holder = std::net::TcpListener::bind(("127.0.0.1", home.https_port)).unwrap();
    let _daemon = home.start();
    assert!(issue(&home, "app.example.com").ok);
    assert_eq!(
        home.get("app.example.com", "/").0,
        404,
        "served, not redirected"
    );
}

#[test]
fn due_certificates_are_renewed_and_served() {
    let mut home = Home::new();
    // Three-second certificates are due after two seconds.
    home.set_env("PROXLET_TEST_FAKE_ACME", "3");
    home.set_env("PROXLET_TEST_RENEW_EVERY_MS", "300");
    let daemon = home.start();
    assert!(issue(&home, "app.example.com").ok);
    let first = not_after(&home, "app.example.com");

    let deadline = Instant::now() + Duration::from_secs(10);
    while not_after(&home, "app.example.com") <= first {
        assert!(Instant::now() < deadline, "never renewed");
        std::thread::sleep(Duration::from_millis(200));
    }
    daemon.wait_for_log(
        "renewed the certificate for app.example.com",
        Duration::from_secs(5),
    );
    assert_eq!(home.https_get("app.example.com", "/").unwrap().0, 404);
}

#[test]
fn a_failing_renewal_keeps_the_old_certificate_and_is_reported() {
    // A certificate that is already due, from a CA of the test's own.
    let ca = proxlet_cert::fake::FakeCa::new();
    let mut home = Home::new();
    let now = proxlet_cert::now();
    let (chain, key) = ca.issue_between("app.example.com", now - 80 * 86_400, now + 10 * 86_400);
    proxlet_cert::Store::new(home.path().join("state/certs"))
        .put("app.example.com", &chain, &key, "production")
        .unwrap();

    home.set_env("PROXLET_TEST_FAKE_ACME_FAIL", "connection refused");
    home.set_env("PROXLET_TEST_RENEW_EVERY_MS", "200");
    let daemon = home.start();
    daemon.wait_for_log(
        "renewing the certificate for app.example.com failed",
        Duration::from_secs(5),
    );

    let out = home.run(&["status"]);
    assert!(
        out.stderr
            .contains("the certificate for app.example.com expires in 9 days")
            || out
                .stderr
                .contains("the certificate for app.example.com expires in 10 days"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("renewal is failing") && out.stderr.contains("connection refused"),
        "{}",
        out.stderr
    );
    assert_eq!(out.code, 0);

    // The old certificate is still served.
    let (status, _) = https_get(home.https_port, ca.root_pem(), "app.example.com", "/").unwrap();
    assert_eq!(status, 404);
    // And the failure is recorded for `cert ls`, with a backoff.
    let meta = proxlet_cert::Store::new(home.path().join("state/certs"))
        .get("app.example.com")
        .unwrap()
        .unwrap()
        .meta;
    assert_eq!(meta.failures, 1, "backs off instead of retrying at once");
}

#[test]
fn renew_now_and_certs_changed() {
    let home = Home::new();
    let _daemon = home.start();
    assert!(issue(&home, "app.example.com").ok);
    let first = not_after(&home, "app.example.com");
    std::thread::sleep(Duration::from_millis(1100));

    let reply = home.control(&Request::Renew {
        domains: vec!["app.example.com".into()],
    });
    let renewed = reply.renewed.unwrap();
    assert_eq!(renewed.len(), 1);
    assert!(renewed[0].not_after.unwrap() > first, "{renewed:?}");

    // Deleting the files and telling the daemon stops HTTPS for that name.
    proxlet_cert::Store::new(home.path().join("state/certs"))
        .remove("app.example.com")
        .unwrap();
    assert!(home.control(&Request::CertsChanged).ok);
    assert!(home.https_get("app.example.com", "/").is_err());
    assert_eq!(
        home.get("app.example.com", "/").0,
        404,
        "no longer redirected"
    );
}

#[test]
fn probe_tokens_are_answered() {
    let home = Home::new();
    let _daemon = home.start();
    let (token, body) = home.control(&Request::Probe).probe.unwrap();
    let (status, got) = home.get(
        "anything.example.com",
        &format!("/.well-known/acme-challenge/{token}"),
    );
    assert_eq!((status, got.as_str()), (200, body.as_str()));
}
