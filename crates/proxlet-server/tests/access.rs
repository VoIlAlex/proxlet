//! Access records, end to end, through a recording sink.
#![allow(missing_docs)]

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::*;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper::body::Frame;
use proxlet_server::{AccessLog, AccessRecord, Connections, Origin, TlsView};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct Recorder(Mutex<Vec<AccessRecord>>);

impl AccessLog for Recorder {
    fn record(&self, record: AccessRecord) {
        self.0.lock().unwrap().push(record);
    }
}

impl Recorder {
    /// Waits until `n` records have arrived and returns them.
    async fn wait(&self, n: usize) -> Vec<AccessRecord> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let got = self.0.lock().unwrap().clone();
            if got.len() >= n || Instant::now() > deadline {
                assert_eq!(got.len(), n, "{got:#?}");
                return got;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

async fn recorded(config: &proxlet_core::Config) -> (Proxlet, Arc<Recorder>) {
    let p = Proxlet::start(config).await;
    let rec = Arc::new(Recorder::default());
    p.engine.set_access_log(rec.clone());
    (p, rec)
}

fn stream_from<T: Send + 'static>(
    mut rx: tokio::sync::mpsc::Receiver<T>,
) -> impl futures_util::Stream<Item = T> + Send + Sync + 'static {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// An upstream that sends `chunks` chunks of 10 bytes, `gap` apart.
async fn slow_upstream(chunks: usize, gap: Duration) -> std::net::SocketAddr {
    upstream(move |_req| async move {
        let (tx, rx) =
            tokio::sync::mpsc::channel::<Result<Frame<Bytes>, std::convert::Infallible>>(1);
        tokio::spawn(async move {
            for i in 0..chunks {
                if i > 0 {
                    tokio::time::sleep(gap).await;
                }
                if tx
                    .send(Ok(Frame::data(Bytes::from(format!("chunk{i:04}\n")))))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Response::new(BodyExt::boxed(StreamBody::new(stream_from(rx))))
    })
    .await
}

#[tokio::test]
async fn proxied_request_without_query() {
    let up = upstream(|_req| async { Response::new(body(vec![b'x'; 512])) }).await;
    let (p, rec) = recorded(&config(&[("/api", &up.port().to_string())], Path::new("/"))).await;
    let got = get(p.addr, "Example.COM:8080", "/api/users?page=2").await;
    assert_eq!(got.status, StatusCode::OK);
    let r = rec.wait(1).await.remove(0);
    assert_eq!(r.method, "GET");
    assert_eq!(r.host.as_deref(), Some("example.com"));
    assert_eq!(r.path, "/api/users");
    assert_eq!(r.status, 200);
    assert_eq!(r.bytes, 512);
    assert_eq!(r.rule.as_deref(), Some("/api"));
    assert_eq!(r.target, Some(format!("127.0.0.1:{}", up.port())));
    assert_eq!(r.client, "127.0.0.1");
    assert_eq!(r.error, None);
    assert!(!format!("{r:?}").contains("page=2"));
}

#[tokio::test]
async fn behind_nginx_the_client_is_the_forwarded_one() {
    let up = upstream(|_req| async { Response::new(body("ok")) }).await;
    let engine = proxlet_server::Engine::new(
        proxlet_server::Snapshot::build(&config(&[("/", &up.port().to_string())], Path::new("/")))
            .unwrap(),
    );
    let rec = Arc::new(Recorder::default());
    engine.set_access_log(rec.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (_stop, rx) = oneshot::channel::<()>();
    tokio::spawn(proxlet_server::accept(
        l,
        engine,
        Connections::new(),
        Origin::Front,
        async {
            let _ = rx.await;
        },
    ));
    let req = Request::get("/")
        .header("host", "site.example")
        .header("x-forwarded-for", "203.0.113.7")
        .body(Empty::new())
        .unwrap();
    assert_eq!(send(addr, req).await.status, StatusCode::OK);
    assert_eq!(rec.wait(1).await[0].client, "203.0.113.7");
}

#[tokio::test]
async fn no_rule_challenge_and_redirect_are_recorded() {
    let (p, rec) = recorded(&config(&[], Path::new("/"))).await;
    assert_eq!(get(p.addr, "x", "/nothing").await.status, 404);
    p.engine.publish_challenge("tok", "tok.key");
    assert_eq!(
        get(p.addr, "x", "/.well-known/acme-challenge/tok")
            .await
            .status,
        200
    );
    p.engine.set_tls_view(TlsView {
        names: ["secure.example".to_owned()].into(),
        https_served: true,
        https_port: 443,
    });
    assert_eq!(get(p.addr, "secure.example", "/a?b").await.status, 308);

    let got = rec.wait(3).await;
    assert_eq!(
        (got[0].status, &got[0].rule, &got[0].target),
        (404, &None, &None)
    );
    assert_eq!(got[0].path, "/nothing");
    assert!(got[0].bytes > 0);
    assert_eq!(got[1].target.as_deref(), Some("acme-challenge"));
    assert_eq!(got[1].rule, None);
    assert_eq!(got[1].bytes, 7);
    assert_eq!(got[2].status, 308);
    assert_eq!(got[2].target.as_deref(), Some("redirect"));
    assert_eq!(got[2].path, "/a");
}

#[tokio::test]
async fn files_rule_names_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (p, rec) = recorded(&config(
        &[("/files", root.to_str().unwrap())],
        Path::new("/"),
    ))
    .await;
    assert_eq!(get(p.addr, "x", "/files/a.txt").await.body, "hello");
    let r = rec.wait(1).await.remove(0);
    assert_eq!(r.rule.as_deref(), Some("/files"));
    assert_eq!(r.target.as_deref(), root.to_str());
    assert_eq!(r.bytes, 5);
}

#[tokio::test]
async fn upstream_down_records_the_reason() {
    let port = closed_port();
    let (p, rec) = recorded(&config(&[("/api", &port.to_string())], Path::new("/"))).await;
    assert_eq!(get(p.addr, "x", "/api").await.status, 502);
    let r = rec.wait(1).await.remove(0);
    assert_eq!(r.status, 502);
    assert_eq!(r.error.as_deref(), Some("connection refused"));
    assert_eq!(r.rule.as_deref(), Some("/api"));
}

#[tokio::test]
async fn streaming_response_is_one_record_with_totals() {
    let up = slow_upstream(4, Duration::from_millis(300)).await;
    let (p, rec) = recorded(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    let got = get(p.addr, "x", "/").await;
    assert_eq!(got.body.len(), 40);
    let r = rec.wait(1).await.remove(0);
    assert_eq!(r.bytes, 40);
    assert!(r.duration >= Duration::from_millis(850), "{:?}", r.duration);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rec.count(), 1);
}

#[tokio::test]
async fn client_hanging_up_mid_stream_is_recorded() {
    let up = slow_upstream(1000, Duration::from_millis(100)).await;
    let (p, rec) = recorded(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    {
        let stream = tokio::net::TcpStream::connect(p.addr).await.unwrap();
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .unwrap();
        let conn = tokio::spawn(conn);
        let resp = sender
            .send_request(
                Request::get("/")
                    .header("host", "x")
                    .body(Empty::<Bytes>::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = resp.into_body();
        BodyExt::frame(&mut body).await.unwrap().unwrap();
        conn.abort();
    }
    let r = rec.wait(1).await.remove(0);
    assert_eq!(r.status, 200);
    assert!(r.bytes >= 10 && r.bytes < 10_000, "{}", r.bytes);
}

#[tokio::test]
async fn websocket_upgrade_is_one_record() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
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
    let (p, rec) = recorded(&config(&[("/ws", &ws_port.to_string())], Path::new("/"))).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", p.addr))
        .await
        .unwrap();
    ws.send(Message::text("hi")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap().to_text().unwrap(), "hi");
    let r = rec.wait(1).await.remove(0);
    assert_eq!(r.status, 101);
    assert_eq!(r.bytes, 0);
    ws.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rec.count(), 1);
}

#[tokio::test]
async fn access_logging_off_applies_live() {
    let up = upstream(|_req| async { Response::new(body("ok")) }).await;
    let mut cfg = config(&[("/", &up.port().to_string())], Path::new("/"));
    let (p, rec) = recorded(&cfg).await;
    get(p.addr, "x", "/").await;
    rec.wait(1).await;
    cfg.settings.log_access = false;
    p.replace(&cfg);
    assert_eq!(get(p.addr, "x", "/").await.status, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rec.count(), 1);
    cfg.settings.log_access = true;
    p.replace(&cfg);
    get(p.addr, "x", "/").await;
    rec.wait(2).await;
}
