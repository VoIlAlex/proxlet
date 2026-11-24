//! Reading and planning against realistic nginx layouts (Debian/Ubuntu, RHEL,
//! Homebrew), built in a temporary directory with absolute paths filled in.
#![allow(missing_docs)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use proxlet_core::command::SystemRunner;
use proxlet_front::apply::Step;
use proxlet_front::{FrontServer, HttpsForward, Installation, Nginx};

struct Layout {
    dir: tempfile::TempDir,
    conf: PathBuf,
}

impl Layout {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, contents: &str) {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(
            p,
            contents.replace("{root}", &self.root().display().to_string()),
        )
        .unwrap();
    }

    fn inst(&self) -> Installation {
        Installation {
            binary: "nginx".into(),
            version: "1.24.0".into(),
            conf_path: self.conf.clone(),
            prefix: self.root().to_owned(),
            default_error_log: Some(self.root().join("log/error.log")),
            ssl: true,
        }
    }
}

fn layout(conf_rel: &str) -> Layout {
    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join(conf_rel);
    Layout { dir, conf }
}

/// Debian/Ubuntu: conf.d and sites-enabled (a symlink into sites-available).
fn debian() -> Layout {
    let l = layout("etc/nginx/nginx.conf");
    l.write(
        "etc/nginx/nginx.conf",
        "user www-data;\n\
         worker_processes auto;\n\
         error_log {root}/var/log/nginx/error.log;\n\
         include {root}/etc/nginx/modules-enabled/*.conf;\n\
         events {\n    worker_connections 768;\n}\n\
         http {\n\
         \tsendfile on;\n\
         \tinclude {root}/etc/nginx/mime.types;\n\
         \tinclude {root}/etc/nginx/conf.d/*.conf;\n\
         \tinclude {root}/etc/nginx/sites-enabled/*;\n\
         }\n",
    );
    l.write("etc/nginx/mime.types", "types {\n    text/html html;\n}\n");
    l.write("etc/nginx/modules-enabled/.keep.conf", "");
    fs::create_dir_all(l.root().join("etc/nginx/conf.d")).unwrap();
    l.write(
        "etc/nginx/sites-available/default",
        "server {\n\
         \tlisten 80 default_server;\n\
         \tlisten [::]:80 default_server;\n\
         \troot /var/www/html;\n\
         \tserver_name _;\n\
         }\n",
    );
    l.write(
        "etc/nginx/sites-available/site",
        "server {\n\
         \tlisten 80;\n\
         \tserver_name site.example www.site.example;\n\
         \tlocation / { return 200 \"site\"; }\n\
         }\n",
    );
    fs::create_dir_all(l.root().join("etc/nginx/sites-enabled")).unwrap();
    for name in ["default", "site"] {
        symlink(
            l.root().join("etc/nginx/sites-available").join(name),
            l.root().join("etc/nginx/sites-enabled").join(name),
        )
        .unwrap();
    }
    l
}

/// RHEL/Fedora: servers in nginx.conf itself, conf.d for extras.
fn rhel() -> Layout {
    let l = layout("etc/nginx/nginx.conf");
    l.write(
        "etc/nginx/nginx.conf",
        "user nginx;\n\
         error_log /var/log/nginx/error.log notice;\n\
         events { worker_connections 1024; }\n\
         http {\n\
         \x20   include conf.d/*.conf;\n\
         \x20   server {\n\
         \x20       listen       80;\n\
         \x20       listen       [::]:80;\n\
         \x20       server_name  _;\n\
         \x20       root         /usr/share/nginx/html;\n\
         \x20   }\n\
         }\n",
    );
    l.write(
        "etc/nginx/conf.d/app.conf",
        "server {\n    listen 80;\n    server_name app.example *.app.example;\n}\n",
    );
    l
}

/// Homebrew on macOS: port 8080 by default, servers/ for extras.
fn homebrew() -> Layout {
    let l = layout("opt/homebrew/etc/nginx/nginx.conf");
    l.write(
        "opt/homebrew/etc/nginx/nginx.conf",
        "worker_processes  1;\n\
         events {\n    worker_connections  1024;\n}\n\
         http {\n\
         \x20   include       mime.types;\n\
         \x20   server {\n\
         \x20       listen       8080;\n\
         \x20       server_name  localhost;\n\
         \x20   }\n\
         \x20   include servers/*;\n\
         }\n",
    );
    l.write("opt/homebrew/etc/nginx/mime.types", "types { }\n");
    fs::create_dir_all(l.root().join("opt/homebrew/etc/nginx/servers")).unwrap();
    l
}

fn nginx() -> Nginx {
    Nginx::new(SystemRunner)
}

#[test]
fn debian_layout() {
    let l = debian();
    let cfg = nginx().read(&l.inst()).unwrap();
    assert_eq!(cfg.servers.len(), 2);
    let dirs: Vec<_> = cfg
        .http_includes
        .iter()
        .map(|i| i.pattern.as_str())
        .collect();
    assert_eq!(dirs, ["*.conf", "*"]);
    assert_eq!(cfg.http_includes[0].dir, l.root().join("etc/nginx/conf.d"));

    let claim = cfg.claims("www.site.example", 80).unwrap();
    assert!(
        claim.at.file.ends_with("sites-enabled/site"),
        "{}",
        claim.at
    );
    assert_eq!(claim.at.line, 3);
    assert!(cfg.claims("other.example", 80).is_none());

    let default = cfg.default_server(80).unwrap();
    assert!(default.at.file.ends_with("sites-enabled/default"));
    assert_eq!(default.at.line, 2);
    assert_eq!(
        nginx().error_log(&l.inst(), &cfg).unwrap(),
        l.root().join("var/log/nginx/error.log")
    );
}

#[test]
fn rhel_layout_with_relative_include() {
    let l = rhel();
    let cfg = nginx().read(&l.inst()).unwrap();
    assert_eq!(cfg.servers.len(), 2);
    assert!(cfg.claims("x.app.example", 80).is_some());
    // `server_name _` is not a wildcard; nothing is the default.
    assert!(cfg.claims("anything.example", 80).is_none());
    assert!(cfg.default_server(80).is_none());
    assert_eq!(cfg.error_log.as_deref(), Some("/var/log/nginx/error.log"));
}

#[test]
fn homebrew_layout() {
    let l = homebrew();
    let cfg = nginx().read(&l.inst()).unwrap();
    assert!(!cfg.listens_on(80));
    assert!(cfg.listens_on(8080));
    assert_eq!(cfg.http_includes[0].pattern, "*");
    assert_eq!(
        nginx().error_log(&l.inst(), &cfg).unwrap(),
        l.root().join("log/error.log")
    );
}

#[test]
fn a_missing_plain_include_is_reported_with_its_location() {
    let l = layout("nginx.conf");
    l.write(
        "nginx.conf",
        "http {\n    include {root}/missing.conf;\n}\n",
    );
    let err = nginx().read(&l.inst()).unwrap_err().to_string();
    assert!(err.contains("missing.conf"), "{err}");
}

#[test]
fn a_glob_matching_nothing_is_fine() {
    let l = layout("nginx.conf");
    l.write("nginx.conf", "http {\n    include {root}/none/*.conf;\n}\n");
    let cfg = nginx().read(&l.inst()).unwrap();
    assert_eq!(cfg.servers, []);
    assert_eq!(cfg.http_includes.len(), 1);
}

#[test]
fn includes_inside_server_blocks_are_followed() {
    let l = layout("nginx.conf");
    l.write(
        "nginx.conf",
        "http {\n  server {\n    include {root}/snippets/listen.conf;\n    server_name inc.example;\n  }\n}\n",
    );
    l.write("snippets/listen.conf", "listen 8081 default_server;\n");
    let cfg = nginx().read(&l.inst()).unwrap();
    assert!(cfg.claims("inc.example", 8081).is_some());
    assert!(cfg.default_server(8081).is_some());
}

fn internal() -> std::net::SocketAddr {
    "127.0.0.1:47080".parse().unwrap()
}

#[test]
fn plan_on_debian_moves_both_default_flags_through_the_symlink() {
    let l = debian();
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    let plan = n.plan_attach(&cfg, 80, None, internal(), true).unwrap();
    let target = fs::canonicalize(l.root().join("etc/nginx/sites-available/default")).unwrap();
    assert_eq!(
        plan.steps[..2],
        [
            Step::EditLine {
                path: target.clone(),
                line: 2,
                from: "\tlisten 80 default_server;".into(),
                to: "\tlisten 80;".into(),
            },
            Step::EditLine {
                path: target,
                line: 3,
                from: "\tlisten [::]:80 default_server;".into(),
                to: "\tlisten [::]:80;".into(),
            },
        ]
    );
    let Step::Write { path, contents } = &plan.steps[2] else {
        panic!("expected a write: {plan:?}");
    };
    assert_eq!(*path, l.root().join("etc/nginx/conf.d/proxlet.conf"));
    assert!(contents.contains("listen [::]:80 default_server;"));
    assert!(contents.contains("proxy_pass http://127.0.0.1:47080;"));

    // Declined: nothing is planned.
    assert!(
        n.plan_attach(&cfg, 80, None, internal(), false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn plan_on_rhel_writes_only_the_block() {
    let l = rhel();
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    let plan = n.plan_attach(&cfg, 80, None, internal(), false).unwrap();
    assert_eq!(plan.steps.len(), 1);
    let Step::Write { path, .. } = &plan.steps[0] else {
        panic!("{plan:?}");
    };
    assert_eq!(*path, l.root().join("etc/nginx/conf.d/proxlet.conf"));
}

#[test]
fn plan_on_homebrew_uses_the_servers_directory() {
    let l = homebrew();
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    let plan = n.plan_attach(&cfg, 80, None, internal(), false).unwrap();
    let Step::Write { path, contents } = &plan.steps[0] else {
        panic!("{plan:?}");
    };
    assert_eq!(
        *path,
        l.root().join("opt/homebrew/etc/nginx/servers/proxlet")
    );
    assert!(!contents.contains("[::]"), "no IPv6 listen in this config");
}

#[test]
fn plan_without_an_included_directory_explains_what_to_add() {
    let l = layout("nginx.conf");
    l.write("nginx.conf", "http {\n  server { listen 80; }\n}\n");
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    let err = n
        .plan_attach(&cfg, 80, None, internal(), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("include /etc/nginx/conf.d/*.conf;"), "{err}");
}

#[test]
fn proxlets_own_block_is_recognised_after_writing() {
    let l = rhel();
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    let plan = n.plan_attach(&cfg, 80, None, internal(), false).unwrap();
    for step in &plan.steps {
        if let Step::Write { path, contents } = step {
            fs::write(path, contents).unwrap();
        }
    }
    let after = n.read(&l.inst()).unwrap();
    assert!(after.has_managed_block());
    // proxlet's own default never counts as someone else's.
    assert!(after.default_server(80).is_none());
    assert!(after.claims("anything.example", 80).is_none());
}

fn https() -> HttpsForward {
    HttpsForward {
        port: 443,
        cert_dir: "/var/lib/proxlet/nginx-certs".into(),
    }
}

#[test]
fn plan_with_https_adds_a_tls_block_that_reads_proxlets_certificates() {
    let l = rhel();
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    let plan = n
        .plan_attach(&cfg, 80, Some(&https()), internal(), false)
        .unwrap();
    let Step::Write { contents, .. } = &plan.steps[0] else {
        panic!("{plan:?}");
    };
    assert!(contents.contains("listen 80 default_server;"), "{contents}");
    assert!(
        contents.contains("listen 443 ssl default_server;"),
        "{contents}"
    );
    assert!(
        contents.contains("ssl_certificate     /var/lib/proxlet/nginx-certs/$proxlet_cert.pem;"),
        "{contents}"
    );
    assert!(contents.contains("~^[A-Za-z0-9.-]+$  $ssl_server_name;"));
    assert_eq!(
        contents
            .matches("proxy_pass http://127.0.0.1:47080;")
            .count(),
        2
    );

    // Written, it is proxlet's own block on both ports.
    fs::write(l.root().join("etc/nginx/conf.d/proxlet.conf"), contents).unwrap();
    let after = n.read(&l.inst()).unwrap();
    assert!(after.has_managed_block());
    assert!(after.default_server(443).is_none());
}

#[test]
fn default_on_port_443_too_moves_both_flags() {
    let l = rhel();
    l.write(
        "etc/nginx/conf.d/main.conf",
        "server {\n    listen 80 default_server;\n    server_name main.example;\n    listen 443 ssl default_server;\n}\n",
    );
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    assert!(cfg.default_server(80).is_some() && cfg.default_server(443).is_some());
    let plan = n
        .plan_attach(&cfg, 80, Some(&https()), internal(), true)
        .unwrap();
    let edited: Vec<usize> = plan
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::EditLine { line, .. } => Some(*line),
            Step::Write { .. } => None,
        })
        .collect();
    assert_eq!(edited, [2, 4]);

    // Declined: both ports keep their default, so nothing is written.
    assert!(
        n.plan_attach(&cfg, 80, Some(&https()), internal(), false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn declining_only_skips_the_port_that_has_a_default() {
    let l = rhel();
    l.write(
        "etc/nginx/conf.d/tls.conf",
        "server {\n    listen 443 ssl default_server;\n    server_name tls.example;\n}\n",
    );
    let n = nginx();
    let cfg = n.read(&l.inst()).unwrap();
    let plan = n
        .plan_attach(&cfg, 80, Some(&https()), internal(), false)
        .unwrap();
    assert_eq!(plan.steps.len(), 1, "no flag is moved: {plan:?}");
    let Step::Write { contents, .. } = &plan.steps[0] else {
        panic!("{plan:?}");
    };
    assert!(contents.contains("listen 80 default_server;"));
    assert!(!contents.contains("443"), "{contents}");
}

#[test]
fn worker_user_and_group_are_read() {
    let l = layout("nginx.conf");
    l.write("nginx.conf", "user www-data;\nhttp {}\n");
    let cfg = nginx().read(&l.inst()).unwrap();
    assert_eq!(cfg.worker_user.as_deref(), Some("www-data"));
    assert_eq!(cfg.worker_group, None);
    l.write("nginx.conf", "user nginx web;\nhttp {}\n");
    let cfg = nginx().read(&l.inst()).unwrap();
    assert_eq!(
        (cfg.worker_user.as_deref(), cfg.worker_group.as_deref()),
        (Some("nginx"), Some("web"))
    );
}
