//! Attach mode from the command line, against a fake nginx (a config tree and
//! a script answering `nginx -V`, `-t` and `-s reload`).
#![allow(missing_docs)]

mod common;

use std::time::Duration;

use common::*;

#[test]
fn conflict_without_a_terminal_adds_nothing() {
    let home = Home::with_nginx(false);
    let out = home.run(&["add", "site.example", "3000"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("nginx serves site.example (")
            && out.stderr.contains("sites-enabled/site:3)")
            && out
                .stderr
                .contains("requests for it will not reach proxlet"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    assert!(
        home.run(&["ls"]).stderr.contains("No rules yet"),
        "a rule was added"
    );
}

#[test]
fn conflict_confirmed_with_yes_is_marked_in_ls() {
    let home = Home::with_nginx(false);
    let out = home.run(&["add", "site.example", "3000", "--yes"]);
    out.ok();
    assert!(
        out.stderr.contains("nginx serves site.example"),
        "{}",
        out.stderr
    );
    home.run(&["add", "/api", "8000"]).ok();
    // Hosts nginx does not serve are not questioned.
    home.run(&["add", "other.example", "8001"]).ok();

    let ls = home.run(&["ls"]).ok().stdout.clone();
    let lines: Vec<&str> = ls.lines().collect();
    assert_eq!(lines.len(), 3, "{ls}");
    for line in &lines {
        assert_eq!(line.split('\t').count(), 4, "{line:?}");
    }
    let site = lines
        .iter()
        .find(|l| l.starts_with("site.example\t"))
        .unwrap();
    assert!(
        site.ends_with(&format!(
            "\tserved by nginx ({}:3)",
            home.path().join("nginx/sites-enabled/site").display()
        )),
        "{site:?}"
    );
    let api = lines.iter().find(|l| l.starts_with("/api")).unwrap();
    assert!(api.ends_with('\t'), "{api:?}");
}

#[test]
fn attach_disabled_reads_no_nginx_config() {
    let home = Home::with_nginx(false);
    home.run(&["config", "attach.enabled", "off"]).ok();
    let out = home.run(&["add", "site.example", "3000"]);
    out.ok();
    assert!(!out.stderr.contains("nginx"), "{}", out.stderr);
    assert!(home.run(&["ls"]).ok().stdout.ends_with("\t\n"));
}

#[test]
fn run_attaches_when_it_may_write_nginx_config_and_detaches_on_exit() {
    let home = Home::with_nginx(false);
    let before = home.nginx_tree();
    let daemon = home.start();
    let block = home.path().join("nginx/conf.d/proxlet.conf");
    assert!(block.exists(), "proxlet did not attach");
    let text = std::fs::read_to_string(&block).unwrap();
    assert!(
        text.contains(&format!("listen {} default_server;", home.port)),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "proxy_pass http://127.0.0.1:{};",
            home.internal_port
        )),
        "{text}"
    );
    assert_eq!(daemon.stop(), 0);
    assert_eq!(home.nginx_tree(), before, "nginx config not restored");
}

#[test]
fn run_without_a_terminal_keeps_nginxs_default_server() {
    let home = Home::with_nginx(true);
    let before = home.nginx_tree();
    let daemon = home.start();
    daemon.wait_for_log("keeps its own default server", Duration::from_secs(5));
    assert_eq!(home.nginx_tree(), before);
}

#[test]
fn behind_nginx_status_and_reclaim() {
    let home = Home::with_nginx(false);
    // Read-only config: `proxlet run` cannot set up the attach itself.
    let conf_d = home.path().join("nginx/conf.d");
    let mode = |m| {
        let perms = <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(m);
        std::fs::set_permissions(&conf_d, perms).unwrap();
    };
    mode(0o555);

    // "nginx" takes the port first: a proxlet binary whose process is named nginx.
    let holder_home = Home::new();
    holder_home
        .run(&["config", "listen.http_port", &home.port.to_string()])
        .ok();
    let nginx = holder_home.start_as(&holder_home.binary_named_nginx());

    let _daemon = home.start();
    let out = home.wait_for_status("mode    attached (nginx)", Duration::from_secs(10));
    assert!(
        out.contains(&format!(
            "http://127.0.0.1:{} (behind nginx on port {})",
            home.internal_port, home.port
        )),
        "{out}"
    );
    let status = home.run(&["status"]);
    assert!(
        status
            .stderr
            .contains("nginx is not forwarding to proxlet yet")
            && status.stderr.contains("sudo proxlet start"),
        "{}",
        status.stderr
    );
    // Requests still reach proxlet on the internal port meanwhile.
    assert_eq!(home.get_internal("x", "/").0, 404);

    // nginx stops: proxlet takes the port back within 5 seconds.
    assert_eq!(nginx.stop(), 0);
    home.wait_for_status("mode    standalone", Duration::from_secs(5));
    assert_eq!(home.get("x", "/").0, 404);
    mode(0o755);
}
