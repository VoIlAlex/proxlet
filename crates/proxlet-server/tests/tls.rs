//! ACME challenges, the redirect to HTTPS, and the HTTPS listener.
#![allow(missing_docs)]

mod common;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode, Version};
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::{TokioExecutor, TokioIo};
use proxlet_cert::fake::FakeCa;
use proxlet_server::tls::{CertResolver, certified_key, server_config};
use proxlet_server::{Connections, Engine, Origin, Snapshot, TlsView};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;

const DAY: i64 = 86_400;

/// An engine with a direct, a front and an HTTPS listener.
struct Server {
    engine: Arc<Engine>,
    resolver: Arc<CertResolver>,
    direct: SocketAddr,
    front: SocketAddr,
    https: SocketAddr,
    _stops: Vec<oneshot::Sender<()>>,
}

async fn bind() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

impl Server {
    async fn start(config: &proxlet_core::Config) -> Self {
        let engine = Engine::new(Snapshot::build(config).unwrap());
        let resolver = CertResolver::new();
        let conns = Connections::new();
        let mut stops = Vec::new();
        let mut stop = || {
            let (tx, rx) = oneshot::channel::<()>();
            stops.push(tx);
            async {
                let _ = rx.await;
            }
        };
        let (l, direct) = bind().await;
        tokio::spawn(proxlet_server::accept(
            l,
            engine.clone(),
            conns.clone(),
            Origin::Direct,
            stop(),
        ));
        let (l, front) = bind().await;
        tokio::spawn(proxlet_server::accept(
            l,
            engine.clone(),
            conns.clone(),
            Origin::Front,
            stop(),
        ));
        let (l, https) = bind().await;
        tokio::spawn(proxlet_server::accept_tls(
            l,
            engine.clone(),
            conns.clone(),
            server_config(resolver.clone()),
            stop(),
        ));
        Self {
            engine,
            resolver,
            direct,
            front,
            https,
            _stops: stops,
        }
    }

    /// Installs certificates for `domains` and marks HTTPS as served on `port`.
    fn certify(&self, ca: &FakeCa, domains: &[&str], port: u16) {
        let mut map = HashMap::new();
        for d in domains {
            let (chain, key) = ca.issue(d, 90 * DAY);
            let pem = format!("{chain}{key}");
            map.insert(
                (*d).to_owned(),
                Arc::new(certified_key(pem.as_bytes()).unwrap()),
            );
        }
        self.resolver.replace(map);
        self.engine.set_tls_view(TlsView {
            names: domains.iter().map(|d| (*d).to_owned()).collect(),
            https_served: true,
            https_port: port,
        });
    }
}

fn connector(ca: &FakeCa, alpn: &[&[u8]]) -> TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(ca.root_pem().as_bytes()).unwrap())
        .unwrap();
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    TlsConnector::from(Arc::new(config))
}

async fn tls_stream(
    addr: SocketAddr,
    ca: &FakeCa,
    name: &str,
    alpn: &[&[u8]],
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(addr).await?;
    connector(ca, alpn)
        .connect(ServerName::try_from(name.to_owned()).unwrap(), tcp)
        .await
}

/// GET over HTTPS/1.1; returns the response and the leaf certificate seen.
async fn https_get(addr: SocketAddr, ca: &FakeCa, name: &str, path: &str) -> (Got, Vec<u8>) {
    let stream = tls_stream(addr, ca, name, &[b"http/1.1"]).await.unwrap();
    let leaf = stream.get_ref().1.peer_certificates().unwrap()[0].to_vec();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    let req = Request::get(path)
        .header("host", name)
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        Got {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        },
        leaf,
    )
}

async fn echo_proto_upstream() -> SocketAddr {
    upstream(|req| async move {
        let proto = req
            .headers()
            .get("x-forwarded-proto")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Response::new(body(format!("{} {proto}", req.uri())))
    })
    .await
}

// ---- 4.1 challenges ----

#[tokio::test]
async fn challenge_answered_on_every_listener_before_rules() {
    let dir = tempfile::tempdir().unwrap();
    let s = Server::start(&config(
        &[("/", dir.path().to_str().unwrap())],
        Path::new("/"),
    ))
    .await;
    s.engine.publish_challenge("tok123", "tok123.thumb");
    for addr in [s.direct, s.front] {
        let got = get(
            addr,
            "app.example.com",
            "/.well-known/acme-challenge/tok123",
        )
        .await;
        assert_eq!(got.status, StatusCode::OK);
        assert_eq!(got.body, "tok123.thumb");
    }
    s.engine.withdraw_challenge("tok123");
    let got = get(
        s.direct,
        "app.example.com",
        "/.well-known/acme-challenge/tok123",
    )
    .await;
    assert_eq!(got.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_token_falls_through_to_the_rules() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".well-known/acme-challenge")).unwrap();
    std::fs::write(
        dir.path().join(".well-known/acme-challenge/abc"),
        "from disk",
    )
    .unwrap();
    let mut cfg = config(&[("/", dir.path().to_str().unwrap())], Path::new("/"));
    cfg.settings.static_dotfiles = true;
    let s = Server::start(&cfg).await;
    s.engine.publish_challenge("other", "other.thumb");
    let got = get(s.direct, "x.example.com", "/.well-known/acme-challenge/abc").await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body, "from disk");
}

// ---- 4.2 redirect ----

#[tokio::test]
async fn redirect_rules() {
    let up = echo_proto_upstream().await;
    let s = Server::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    let ca = FakeCa::new();
    s.certify(&ca, &["example.com"], 443);

    // Redirected, keeping path and query; the Host's port is dropped.
    let got = get(s.direct, "example.com:8080", "/a?b=1").await;
    assert_eq!(got.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(got.headers["location"], "https://example.com/a?b=1");

    // A host without a certificate is served as before.
    let got = get(s.direct, "other.example.com", "/x").await;
    assert_eq!(got.status, StatusCode::OK);

    // Challenge requests are never redirected.
    s.engine.publish_challenge("t", "t.k");
    let got = get(s.direct, "example.com", "/.well-known/acme-challenge/t").await;
    assert_eq!((got.status, got.body.as_str()), (StatusCode::OK, "t.k"));

    // From nginx: redirect plain HTTP, but not what nginx already got over HTTPS.
    let req = |proto: &str| {
        Request::get("/p")
            .header("host", "example.com")
            .header("x-forwarded-for", "203.0.113.7")
            .header("x-forwarded-proto", proto)
            .body(Empty::new())
            .unwrap()
    };
    let got = send(s.front, req("http")).await;
    assert_eq!(got.status, StatusCode::PERMANENT_REDIRECT);
    let got = send(s.front, req("https")).await;
    assert_eq!(
        (got.status, got.body.as_str()),
        (StatusCode::OK, "/p https")
    );

    // A non-443 HTTPS port appears in the Location.
    s.certify(&ca, &["example.com"], 8443);
    let got = get(s.direct, "example.com", "/").await;
    assert_eq!(got.headers["location"], "https://example.com:8443/");

    // HTTPS not available: no redirect.
    s.engine.set_tls_view(TlsView {
        names: HashSet::from(["example.com".to_owned()]),
        https_served: false,
        https_port: 443,
    });
    assert_eq!(
        get(s.direct, "example.com", "/").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn redirect_off() {
    let up = echo_proto_upstream().await;
    let mut cfg = config(&[("/", &up.port().to_string())], Path::new("/"));
    cfg.settings.tls_redirect = false;
    let s = Server::start(&cfg).await;
    s.certify(&FakeCa::new(), &["example.com"], 443);
    assert_eq!(
        get(s.direct, "example.com", "/").await.status,
        StatusCode::OK
    );
}

// ---- 4.3 TLS ----

#[tokio::test]
async fn https_picks_the_certificate_and_forwards_https() {
    let up = echo_proto_upstream().await;
    let s = Server::start(&config(
        &[
            ("example.com/", &up.port().to_string()),
            ("/", &up.port().to_string()),
        ],
        Path::new("/"),
    ))
    .await;
    let ca = FakeCa::new();
    s.certify(&ca, &["example.com", "b.example.com"], 443);

    let (got, leaf_a) = https_get(s.https, &ca, "example.com", "/x").await;
    assert_eq!(
        (got.status, got.body.as_str()),
        (StatusCode::OK, "/x https")
    );
    let (_, leaf_b) = https_get(s.https, &ca, "b.example.com", "/").await;
    assert_ne!(leaf_a, leaf_b, "each name gets its own certificate");
    // Never redirected once on HTTPS.
    assert_eq!(got.headers.get("location"), None);
}

#[tokio::test]
async fn unknown_name_or_no_sni_fails_the_handshake() {
    let s = Server::start(&config(&[], Path::new("/"))).await;
    let ca = FakeCa::new();
    s.certify(&ca, &["example.com"], 443);
    assert!(
        tls_stream(s.https, &ca, "other.example.com", &[])
            .await
            .is_err(),
        "unknown name"
    );
    // An IP address sends no SNI.
    let tcp = TcpStream::connect(s.https).await.unwrap();
    let ip = ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into());
    assert!(
        connector(&ca, &[]).connect(ip, tcp).await.is_err(),
        "no SNI"
    );
    // The listener still works afterwards.
    assert!(tls_stream(s.https, &ca, "example.com", &[]).await.is_ok());
}

#[tokio::test]
async fn http2_is_negotiated() {
    let up = echo_proto_upstream().await;
    let s = Server::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    let ca = FakeCa::new();
    s.certify(&ca, &["example.com"], 443);
    let stream = tls_stream(s.https, &ca, "example.com", &[b"h2", b"http/1.1"])
        .await
        .unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(conn);
    let req = Request::get("https://example.com/h2")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.version(), Version::HTTP_2);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"/h2 https");
}

#[tokio::test]
async fn websocket_over_https() {
    let ws_up = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_up.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = ws_up.accept().await {
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    if msg.is_text() {
                        ws.send(msg).await.unwrap();
                    }
                }
            });
        }
    });
    let s = Server::start(&config(&[("/ws", &ws_port.to_string())], Path::new("/"))).await;
    let ca = FakeCa::new();
    s.certify(&ca, &["example.com"], 443);
    let stream = tls_stream(s.https, &ca, "example.com", &[b"http/1.1"])
        .await
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("wss://example.com/ws", stream)
        .await
        .unwrap();
    ws.send(Message::text("hello")).await.unwrap();
    let echoed = ws.next().await.unwrap().unwrap();
    assert_eq!(echoed.into_text().unwrap(), "hello");
}

#[tokio::test]
async fn swapping_certificates_keeps_open_connections() {
    let up = echo_proto_upstream().await;
    let s = Server::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    let ca = FakeCa::new();
    s.certify(&ca, &["example.com"], 443);

    let stream = tls_stream(s.https, &ca, "example.com", &[b"http/1.1"])
        .await
        .unwrap();
    let old_leaf = stream.get_ref().1.peer_certificates().unwrap()[0].to_vec();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    let get = |p: &'static str| {
        Request::get(p)
            .header("host", "example.com")
            .body(Empty::<Bytes>::new())
            .unwrap()
    };
    assert_eq!(sender.send_request(get("/1")).await.unwrap().status(), 200);

    // Renewed: a new certificate for the same name.
    s.certify(&ca, &["example.com"], 443);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        sender.send_request(get("/2")).await.unwrap().status(),
        200,
        "the open connection keeps working"
    );
    let (_, new_leaf) = https_get(s.https, &ca, "example.com", "/3").await;
    assert_ne!(
        old_leaf, new_leaf,
        "the next handshake gets the new certificate"
    );

    // Removed: new handshakes fail.
    s.resolver.replace(HashMap::new());
    assert!(tls_stream(s.https, &ca, "example.com", &[]).await.is_err());
}
