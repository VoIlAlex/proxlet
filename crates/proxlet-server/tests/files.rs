//! Static file serving, end to end.
#![allow(missing_docs)]

mod common;

use std::fs;

use bytes::Bytes;
use common::*;
use http::{Request, StatusCode};
use http_body_util::Empty;

struct Site {
    dir: tempfile::TempDir,
}

impl Site {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("site");
        fs::create_dir_all(root.join("img")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::write(root.join("img/logo.png"), b"PNG fake").unwrap();
        fs::write(root.join("hello.txt"), "0123456789").unwrap();
        fs::write(root.join("docs/guide.md"), "# guide").unwrap();
        fs::write(root.join("docs/<odd>.txt"), "odd").unwrap();
        fs::write(root.join("app/index.html"), "<h1>app</h1>").unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::write(dir.path().join("outside.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside.txt"), root.join("escape.txt"))
            .unwrap();
        std::os::unix::fs::symlink(root.join("hello.txt"), root.join("inside-link.txt")).unwrap();
        Self { dir }
    }

    fn config(&self) -> proxlet_core::Config {
        config(&[("/files", "site")], self.dir.path())
    }
}

async fn start(site: &Site, tweak: impl FnOnce(&mut proxlet_core::Config)) -> Proxlet {
    let mut cfg = site.config();
    tweak(&mut cfg);
    Proxlet::start(&cfg).await
}

#[tokio::test]
async fn file_under_prefix_with_content_type() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let got = get(p.addr, "x", "/files/img/logo.png").await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.headers["content-type"], "image/png");
    assert_eq!(got.body.as_bytes(), b"PNG fake");

    let got = get(p.addr, "x", "/files/hello.txt").await;
    assert_eq!(got.headers["content-type"], "text/plain; charset=utf-8");
    assert_eq!(got.headers["content-length"], "10");
}

#[tokio::test]
async fn missing_file_is_404() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    assert_eq!(
        get(p.addr, "x", "/files/nope.txt").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn range_requests() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let req = |range: &'static str| {
        Request::get("/files/hello.txt")
            .header("host", "x")
            .header("range", range)
            .body(Empty::<Bytes>::new())
            .unwrap()
    };
    let got = send(p.addr, req("bytes=2-5")).await;
    assert_eq!(got.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(got.body, "2345");
    assert_eq!(got.headers["content-range"], "bytes 2-5/10");

    let got = send(p.addr, req("bytes=-3")).await;
    assert_eq!(got.body, "789");

    let got = send(p.addr, req("bytes=50-")).await;
    assert_eq!(got.status, StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn conditional_requests() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let first = get(p.addr, "x", "/files/hello.txt").await;
    let etag = first.headers["etag"].to_str().unwrap().to_owned();
    let last_modified = first.headers["last-modified"].to_str().unwrap().to_owned();

    let got = send(
        p.addr,
        Request::get("/files/hello.txt")
            .header("host", "x")
            .header("if-none-match", &etag)
            .body(Empty::new())
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, StatusCode::NOT_MODIFIED);
    assert_eq!(got.body, "");

    let got = send(
        p.addr,
        Request::get("/files/hello.txt")
            .header("host", "x")
            .header("if-modified-since", &last_modified)
            .body(Empty::new())
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn head_has_headers_but_no_body() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let got = send(
        p.addr,
        Request::head("/files/hello.txt")
            .header("host", "x")
            .body(Empty::new())
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.headers["content-length"], "10");
    assert_eq!(got.body, "");
}

#[tokio::test]
async fn index_file_is_served_for_directories() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let got = get(p.addr, "x", "/files/app/").await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body, "<h1>app</h1>");
    assert_eq!(got.headers["content-type"], "text/html; charset=utf-8");
}

#[tokio::test]
async fn directory_without_slash_redirects() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let got = get(p.addr, "x", "/files/app?x=1").await;
    assert_eq!(got.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(got.headers["location"], "/files/app/?x=1");
    let got = get(p.addr, "x", "/files").await;
    assert_eq!(got.headers["location"], "/files/");
}

#[tokio::test]
async fn listing_on_by_default() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let got = get(p.addr, "x", "/files/").await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(got.body.contains("Index of /files/"), "{}", got.body);
    for entry in ["img/", "docs/", "app/", "hello.txt"] {
        assert!(
            got.body.contains(&format!(">{entry}<")),
            "missing {entry}: {}",
            got.body
        );
    }
    assert!(!got.body.contains(".env"), "dotfile listed: {}", got.body);

    let got = get(p.addr, "x", "/files/docs/").await;
    assert!(got.body.contains("&lt;odd&gt;.txt"), "{}", got.body);
    assert!(got.body.contains("href=\"%3Codd%3E.txt\""), "{}", got.body);
}

#[tokio::test]
async fn listing_off_is_404() {
    let site = Site::new();
    let p = start(&site, |c| c.settings.static_listing = false).await;
    assert_eq!(
        get(p.addr, "x", "/files/docs/").await.status,
        StatusCode::NOT_FOUND
    );
    // Index files still work with listings off.
    assert_eq!(get(p.addr, "x", "/files/app/").await.status, StatusCode::OK);
}

#[tokio::test]
async fn listing_setting_applies_live() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    assert_eq!(
        get(p.addr, "x", "/files/docs/").await.status,
        StatusCode::OK
    );
    let mut cfg = site.config();
    cfg.settings.static_listing = false;
    p.replace(&cfg);
    assert_eq!(
        get(p.addr, "x", "/files/docs/").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn path_traversal_never_leaves_the_directory() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    for path in [
        "/files/../outside.txt",
        "/files/../../etc/passwd",
        "/files/%2e%2e/outside.txt",
        "/files/..%2foutside.txt",
        "/files/img/..%2F..%2Foutside.txt",
        "/files/%2e%2e%5coutside.txt",
    ] {
        let resp = raw(
            p.addr,
            &format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
        )
        .await;
        assert!(
            resp.starts_with("HTTP/1.1 404"),
            "{path} was not rejected:\n{resp}"
        );
        assert!(!resp.contains("outside"), "{path} leaked:\n{resp}");
    }
}

#[tokio::test]
async fn symlink_out_of_the_directory_is_refused() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let got = get(p.addr, "x", "/files/escape.txt").await;
    assert_eq!(got.status, StatusCode::NOT_FOUND);
    assert!(!got.body.contains("outside"));
    // Symlinks that stay inside are fine.
    assert_eq!(
        get(p.addr, "x", "/files/inside-link.txt").await.body,
        "0123456789"
    );
}

#[tokio::test]
async fn dotfiles_hidden_by_default_and_served_when_enabled() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    assert_eq!(
        get(p.addr, "x", "/files/.env").await.status,
        StatusCode::NOT_FOUND
    );

    let p = start(&site, |c| c.settings.static_dotfiles = true).await;
    let got = get(p.addr, "x", "/files/.env").await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(get(p.addr, "x", "/files/").await.body.contains(".env"));
}

#[tokio::test]
async fn non_get_methods_are_refused() {
    let site = Site::new();
    let p = start(&site, |_| {}).await;
    let got = send(
        p.addr,
        Request::post("/files/hello.txt")
            .header("host", "x")
            .body(Empty::new())
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(got.headers["allow"], "GET, HEAD");
}

#[tokio::test]
async fn root_files_rule() {
    let site = Site::new();
    let cfg = config(&[("/", "site")], site.dir.path());
    let p = Proxlet::start(&cfg).await;
    assert_eq!(get(p.addr, "x", "/hello.txt").await.body, "0123456789");
    assert_eq!(get(p.addr, "x", "/app/").await.body, "<h1>app</h1>");
}
