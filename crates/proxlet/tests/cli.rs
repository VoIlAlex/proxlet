//! `add`, `rm`, `ls`, `config` and completion, with proxlet stopped and running.
#![allow(missing_docs)]

mod common;

use common::*;

fn listening_upstream() -> (std::net::TcpListener, u16) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

#[test]
fn add_while_stopped_saves_and_says_so() {
    let home = Home::new();
    let (_up, port) = listening_upstream();
    let out = home.run(&["add", "/api", &port.to_string()]);
    out.ok();
    assert!(
        out.stdout
            .contains(&format!("Added /api → http://127.0.0.1:{port}")),
        "{}",
        out.stdout
    );
    assert!(
        out.stderr.contains("applies when it starts"),
        "{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("Nothing is listening"),
        "{}",
        out.stderr
    );
    assert!(home.run(&["ls"]).ok().stdout.contains("/api\tproxy"));
}

#[test]
fn add_while_running_applies_immediately() {
    let home = Home::new();
    let _daemon = home.start();
    let site = home.path().join("site");
    std::fs::create_dir(&site).unwrap();
    std::fs::write(site.join("a.txt"), "A").unwrap();
    assert_eq!(home.get("x", "/files/a.txt").0, 404);

    let out = home.run(&["add", "/files", "./site"]);
    out.ok();
    assert!(
        !out.stderr.contains("applies when it starts"),
        "{}",
        out.stderr
    );
    assert_eq!(home.get("x", "/files/a.txt"), (200, "A".into()));
}

#[test]
fn rule_survives_restart() {
    let home = Home::new();
    let site = home.path().join("site");
    std::fs::create_dir(&site).unwrap();
    std::fs::write(site.join("a.txt"), "A").unwrap();
    {
        let _daemon = home.start();
        home.run(&["add", "/files", "site"]).ok();
    }
    let _daemon = home.start();
    assert!(home.run(&["ls"]).ok().stdout.contains("/files"));
    assert_eq!(home.get("x", "/files/a.txt"), (200, "A".into()));
}

#[test]
fn add_warns_when_nothing_listens() {
    let home = Home::new();
    let port = free_port();
    let out = home.run(&["add", "/api", &port.to_string()]);
    out.ok();
    assert!(
        out.stderr
            .contains(&format!("Nothing is listening on 127.0.0.1:{port} yet")),
        "{}",
        out.stderr
    );
}

#[test]
fn add_rejects_bad_input_and_changes_nothing() {
    let home = Home::new();
    let before = std::fs::read(home.config_file()).unwrap();
    for (args, needle) in [
        (
            vec!["add", "exa mple.com/api", "8000"],
            "not a valid host name",
        ),
        (vec!["add", "/files", "./nope"], "nope` does not exist"),
        (vec!["add", "/api", "70000"], "not a valid port"),
    ] {
        let out = home.run(&args);
        assert_eq!(out.code, 1, "{args:?}");
        assert!(out.stderr.contains(needle), "{args:?}: {}", out.stderr);
    }
    assert_eq!(std::fs::read(home.config_file()).unwrap(), before);
}

#[test]
fn add_existing_route_updates_it() {
    let home = Home::new();
    home.run(&["add", "/api", "8000"]).ok();
    let out = home.run(&["add", "/api/", "9000"]);
    out.ok();
    assert!(
        out.stdout
            .contains("Updated /api → http://127.0.0.1:9000 (was http://127.0.0.1:8000)"),
        "{}",
        out.stdout
    );
    let ls = home.run(&["ls"]).ok().stdout.clone();
    assert_eq!(ls.lines().count(), 1, "{ls}");
}

#[test]
fn numeric_directory_with_dot_slash_is_a_directory() {
    let home = Home::new();
    std::fs::create_dir(home.path().join("8000")).unwrap();
    home.run(&["add", "/files", "./8000"]).ok();
    let ls = home.run(&["ls"]).ok().stdout.clone();
    assert!(ls.contains("/files\tfiles\t"), "{ls}");
}

#[test]
fn rm_existing_and_unknown() {
    let home = Home::new();
    let _daemon = home.start();
    let (_up, port) = listening_upstream();
    home.run(&["add", "example.com/api", &port.to_string()])
        .ok();
    let out = home.run(&["rm", "example.com/api"]);
    out.ok();
    assert!(
        out.stdout.contains("Removed example.com/api"),
        "{}",
        out.stdout
    );
    assert_eq!(home.get("example.com", "/api").0, 404);

    let out = home.run(&["rm", "/nope"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("no rule for `/nope`"), "{}", out.stderr);
}

#[test]
fn ls_plain_output_and_empty_case() {
    let home = Home::new();
    let out = home.run(&["ls"]);
    out.ok();
    assert_eq!(out.stdout, "");
    assert!(out.stderr.contains("No rules yet"), "{}", out.stderr);
    assert!(out.stderr.contains("proxlet add"), "{}", out.stderr);

    let site = home.path().join("site");
    std::fs::create_dir(&site).unwrap();
    home.run(&["add", "/files", "site"]).ok();
    home.run(&["add", "example.com/api", "8002"]).ok();
    home.run(&["add", "/api", "8000"]).ok();
    let site = site.canonicalize().unwrap();
    assert_eq!(
        home.run(&["ls"]).ok().stdout,
        format!(
            "/api\tproxy\thttp://127.0.0.1:8000\t\n/files\tfiles\t{}\t\nexample.com/api\tproxy\thttp://127.0.0.1:8002\t\n",
            site.display()
        )
    );
}

#[test]
fn config_list_read_and_defaults() {
    let home = Home::new();
    let out = home.run(&["config"]);
    out.ok();
    assert!(
        out.stdout.contains("static.listing\ton\n"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("proxy.timeout\t1m\n"), "{}", out.stdout);
    assert_eq!(home.run(&["config", "static.listing"]).ok().stdout, "on\n");
    assert_eq!(
        home.run(&["config", "proxy.connect_timeout"]).ok().stdout,
        "5s\n"
    );
}

#[test]
fn config_unknown_key_and_invalid_value() {
    let home = Home::new();
    let out = home.run(&["config", "static.listng"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("did you mean `static.listing`"),
        "{}",
        out.stderr
    );

    let before = std::fs::read(home.config_file()).unwrap();
    let out = home.run(&["config", "static.listing", "maybe"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("`on` or `off`"), "{}", out.stderr);
    assert_eq!(std::fs::read(home.config_file()).unwrap(), before);
}

#[test]
fn config_change_applies_live() {
    let home = Home::new();
    let site = home.path().join("site");
    std::fs::create_dir_all(site.join("sub")).unwrap();
    home.run(&["add", "/files", "site"]).ok();
    let _daemon = home.start();
    assert_eq!(home.get("x", "/files/sub/").0, 200);
    let out = home.run(&["config", "static.listing", "off"]);
    out.ok();
    assert!(
        out.stdout.contains("static.listing = off (was on)"),
        "{}",
        out.stdout
    );
    assert_eq!(home.get("x", "/files/sub/").0, 404);
    assert_eq!(home.run(&["config", "static.listing"]).ok().stdout, "off\n");
}

#[test]
fn config_restart_needed_notice() {
    let home = Home::new();
    let _daemon = home.start();
    let out = home.run(&["config", "listen.http_port", &free_port().to_string()]);
    out.ok();
    assert!(
        out.stderr.contains("takes effect after a restart"),
        "{}",
        out.stderr
    );
    // Still serving on the old port.
    assert_eq!(home.get("x", "/").0, 404);
}

#[test]
fn attach_port_needs_a_restart() {
    let home = Home::new();
    let _daemon = home.start();
    let out = home.run(&["config", "attach.port", &free_port().to_string()]);
    out.ok();
    assert!(
        out.stderr.contains("takes effect after a restart"),
        "{}",
        out.stderr
    );
}

#[test]
fn https_port_needs_a_restart() {
    let home = Home::new();
    let _daemon = home.start();
    let out = home.run(&["config", "listen.https_port", &free_port().to_string()]);
    out.ok();
    assert!(
        out.stderr.contains("takes effect after a restart"),
        "{}",
        out.stderr
    );
}

#[test]
fn ports_must_differ_and_nothing_changes() {
    let home = Home::new();
    let http = home.port.to_string();
    let before = home
        .run(&["config", "listen.https_port"])
        .ok()
        .stdout
        .clone();

    let out = home.run(&["config", "listen.https_port", &http]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("the HTTPS port must differ from the HTTP port"),
        "{}",
        out.stderr
    );
    assert_eq!(
        home.run(&["config", "listen.https_port"]).ok().stdout,
        before
    );

    // The reverse: moving the HTTP port onto the HTTPS port.
    let out = home.run(&["config", "listen.http_port", before.trim()]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("the HTTP port must differ from the HTTPS port"),
        "{}",
        out.stderr
    );
    assert_eq!(
        home.run(&["config", "listen.http_port"]).ok().stdout,
        format!("{http}\n")
    );
}

#[test]
fn days_and_sizes() {
    let home = Home::new();
    home.run(&["config", "log.max_age", "30d"]).ok();
    home.run(&["config", "log.max_size", "1GB"]).ok();
    assert_eq!(home.run(&["config", "log.max_age"]).ok().stdout, "30d\n");
    assert_eq!(home.run(&["config", "log.max_size"]).ok().stdout, "1GB\n");
    let listed = home.run(&["config"]).ok().stdout.clone();
    assert!(listed.contains("log.max_age\t30d\n"), "{listed}");
    assert!(listed.contains("log.max_size\t1GB\n"), "{listed}");
}

#[test]
fn log_size_too_small() {
    let home = Home::new();
    let before = std::fs::read(home.config_file()).ok();
    let out = home.run(&["config", "log.max_size", "1MB"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("at least 10MB"), "{}", out.stderr);
    assert_eq!(std::fs::read(home.config_file()).ok(), before);
    assert_eq!(home.run(&["config", "log.max_size"]).ok().stdout, "100MB\n");
}

#[test]
fn acme_server_values() {
    let home = Home::new();
    let out = home.run(&["config", "cert.server", "http://acme.local/dir"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("must be `production`, `staging` or an `https://` ACME directory URL"),
        "{}",
        out.stderr
    );
    home.run(&["config", "cert.server", "staging"]).ok();
    assert_eq!(
        home.run(&["config", "cert.server"]).ok().stdout,
        "staging\n"
    );
    home.run(&["config", "tls.redirect", "off"]).ok();
    assert_eq!(home.run(&["config", "tls.redirect"]).ok().stdout, "off\n");
}

#[test]
fn config_duration_value() {
    let home = Home::new();
    home.run(&["config", "proxy.timeout", "2m"]).ok();
    assert_eq!(home.run(&["config", "proxy.timeout"]).ok().stdout, "2m\n");
}

#[test]
fn proxy_timeout_setting_bounds_upstream_wait() {
    let home = Home::new();
    // An upstream that accepts connections but never answers.
    let (stalled, port) = listening_upstream();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((conn, _)) = stalled.accept() {
            held.push(conn);
        }
    });
    home.run(&["add", "/slow", &port.to_string()]).ok();
    let _daemon = home.start();
    home.run(&["config", "proxy.timeout", "300ms"]).ok();
    let started = std::time::Instant::now();
    let (status, body) = home.get("x", "/slow");
    assert_eq!(status, 504, "{body}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn completion_offers_rules_keys_and_values() {
    let home = Home::new();
    home.run(&["add", "/api", "8000"]).ok();
    home.run(&["add", "example.com/api", "8002"]).ok();
    assert_eq!(home.complete(&["rm", ""]), ["/api", "example.com/api"]);
    assert_eq!(home.complete(&["rm", "ex"]), ["example.com/api"]);
    assert_eq!(
        home.complete(&["config", "static."]),
        ["static.listing", "static.dotfiles"]
    );
    assert_eq!(
        home.complete(&["config", "static.listing", ""]),
        ["on", "off"]
    );
    assert!(home.complete(&[""]).contains(&"add".to_owned()));
}

#[test]
fn not_yet_available_commands_say_so() {
    let home = Home::new();
    let out = home.run(&["monitor"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("not available yet"), "{}", out.stderr);
}
