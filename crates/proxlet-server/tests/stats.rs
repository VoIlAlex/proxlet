//! Live statistics per rule, end to end through the engine.
#![allow(missing_docs)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::*;
use futures_util::{SinkExt, StreamExt};
use http::{Response, StatusCode};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use proxlet_server::{Engine, Totals};
use tokio_tungstenite::tungstenite::Message;

fn row(engine: &Engine, rule: Option<&str>) -> Totals {
    engine
        .stats()
        .rows
        .into_iter()
        .find(|r| r.rule.as_deref() == rule)
        .unwrap_or_else(|| panic!("no row {rule:?}"))
        .totals
}

/// Waits until `check` holds for the row, or fails after 5 seconds.
async fn eventually(engine: &Engine, rule: Option<&str>, check: impl Fn(&Totals) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let t = row(engine, rule);
        if check(&t) {
            return;
        }
        assert!(Instant::now() < deadline, "{rule:?}: {t:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn ms(d: Option<Duration>) -> f64 {
    d.expect("latency").as_secs_f64() * 1000.0
}

fn stream_from<T: Send + 'static>(
    mut rx: tokio::sync::mpsc::Receiver<T>,
) -> impl futures_util::Stream<Item = T> + Send + Sync + 'static {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

#[tokio::test]
async fn requests_counted_per_rule() {
    let api = upstream(|_req| async { Response::new(body("api")) }).await;
    let site = upstream(|_req| async { Response::new(body("site")) }).await;
    let p = Proxlet::start(&config(
        &[
            ("/api", &api.port().to_string()),
            ("/", &site.port().to_string()),
        ],
        Path::new("/"),
    ))
    .await;
    for _ in 0..3 {
        assert_eq!(get(p.addr, "x", "/api/users").await.status, 200);
    }
    for _ in 0..2 {
        assert_eq!(get(p.addr, "x", "/index.html").await.status, 200);
    }
    eventually(&p.engine, Some("/api"), |t| t.total == 3).await;
    eventually(&p.engine, Some("/"), |t| t.total == 2).await;
    assert_eq!(row(&p.engine, None).total, 0);
    let rows: Vec<_> = p.engine.stats().rows.into_iter().map(|r| r.rule).collect();
    assert_eq!(rows, [Some("/".to_owned()), Some("/api".to_owned()), None]);
}

#[tokio::test]
async fn unmatched_requests_go_to_the_no_rule_row() {
    let site = upstream(|_req| async { Response::new(body("site")) }).await;
    let p = Proxlet::start(&config(
        &[("example.com/", &site.port().to_string())],
        Path::new("/"),
    ))
    .await;
    assert_eq!(get(p.addr, "other.example", "/").await.status, 404);
    eventually(&p.engine, None, |t| t.total == 1 && t.status_4xx == 1).await;
    assert_eq!(row(&p.engine, Some("example.com")).total, 0);
}

#[tokio::test]
async fn counted_with_access_logging_off() {
    let up = upstream(|_req| async { Response::new(body("ok")) }).await;
    let mut cfg = config(&[("/", &up.port().to_string())], Path::new("/"));
    cfg.settings.log_access = false;
    let p = Proxlet::start(&cfg).await;
    get(p.addr, "x", "/").await;
    eventually(&p.engine, Some("/"), |t| t.total == 1).await;
}

#[tokio::test]
async fn upstream_errors_are_counted() {
    let port = closed_port();
    let p = Proxlet::start(&config(&[("/api", &port.to_string())], Path::new("/"))).await;
    for _ in 0..4 {
        assert_eq!(get(p.addr, "x", "/api").await.status, 502);
    }
    eventually(&p.engine, Some("/api"), |t| {
        (t.total, t.status_5xx, t.upstream_errors) == (4, 4, 4)
    })
    .await;
}

#[tokio::test]
async fn an_upstreams_own_500_is_not_an_upstream_error() {
    let up = upstream(|_req| async {
        let mut r = Response::new(body("boom"));
        *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        r
    })
    .await;
    let p = Proxlet::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    assert_eq!(get(p.addr, "x", "/").await.status, 500);
    eventually(&p.engine, Some("/"), |t| t.status_5xx == 1).await;
    assert_eq!(row(&p.engine, Some("/")).upstream_errors, 0);
}

#[tokio::test]
async fn latency_percentiles() {
    let up = upstream(|_req| async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Response::new(body("slow"))
    })
    .await;
    let p = Proxlet::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    futures_util::future::join_all((0..10).map(|_| get(p.addr, "x", "/"))).await;
    eventually(&p.engine, Some("/"), |t| t.total == 10).await;
    let t = row(&p.engine, Some("/"));
    for (name, v) in [("p50", t.p50), ("p95", t.p95), ("p99", t.p99)] {
        let v = ms(v);
        assert!((180.0..=260.0).contains(&v), "{name} = {v}ms");
    }
}

#[tokio::test]
async fn latency_is_time_to_the_response_head() {
    // The head comes at once; the body streams for about 2 seconds.
    let up = upstream(|_req| async {
        let (tx, rx) =
            tokio::sync::mpsc::channel::<Result<Frame<Bytes>, std::convert::Infallible>>(1);
        tokio::spawn(async move {
            for _ in 0..10 {
                if tx
                    .send(Ok(Frame::data(Bytes::from(vec![b'x'; 1000]))))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        Response::new(BodyExt::boxed(StreamBody::new(stream_from(rx))))
    })
    .await;
    let p = Proxlet::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    let got = get(p.addr, "x", "/").await;
    assert_eq!(got.body.len(), 10_000);
    let t = row(&p.engine, Some("/"));
    assert!(ms(t.p99) < 100.0, "{:?}", t.p99);
    // Once a complete second has passed, the bytes show in the rate.
    eventually(&p.engine, Some("/"), |t| t.out_bps > 0.0).await;
}

#[tokio::test]
async fn request_in_flight() {
    let up = upstream(|_req| async {
        tokio::time::sleep(Duration::from_millis(800)).await;
        Response::new(body("late"))
    })
    .await;
    let p = Proxlet::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    let addr = p.addr;
    let waiting = tokio::spawn(async move { get(addr, "x", "/").await });
    eventually(&p.engine, Some("/"), |t| t.in_flight == 1).await;
    assert_eq!(waiting.await.unwrap().status, 200);
    eventually(&p.engine, Some("/"), |t| t.in_flight == 0 && t.total == 1).await;
}

#[tokio::test]
async fn open_websockets_are_counted() {
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
    let p = Proxlet::start(&config(&[("/ws", &ws_port.to_string())], Path::new("/"))).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", p.addr))
        .await
        .unwrap();
    ws.send(Message::text("hi")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap().to_text().unwrap(), "hi");
    eventually(&p.engine, Some("/ws"), |t| {
        t.websockets == 1 && t.total == 1
    })
    .await;
    ws.close(None).await.unwrap();
    drop(ws);
    eventually(&p.engine, Some("/ws"), |t| t.websockets == 0).await;
}

#[tokio::test]
async fn open_connections_are_counted() {
    let p = Proxlet::start(&config(&[], Path::new("/"))).await;
    assert_eq!(p.engine.stats().connections, 0);
    let conn = tokio::net::TcpStream::connect(p.addr).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.engine.stats().connections != 1 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(conn);
    while p.engine.stats().connections != 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn statistics_survive_a_rule_change() {
    let up = upstream(|_req| async { Response::new(body("ok")) }).await;
    let mut cfg = config(&[("/api", &up.port().to_string())], Path::new("/"));
    let p = Proxlet::start(&cfg).await;
    get(p.addr, "x", "/api").await;
    eventually(&p.engine, Some("/api"), |t| t.total == 1).await;
    cfg.settings.log_access = false;
    p.replace(&cfg);
    assert_eq!(row(&p.engine, Some("/api")).total, 1);
}
