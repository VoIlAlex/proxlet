//! Reverse proxy behaviour, end to end.
#![allow(missing_docs)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::*;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper::body::Frame;
use tokio_tungstenite::tungstenite::Message;

/// An upstream that echoes what it received as `METHOD URI` plus selected headers.
async fn echo_upstream() -> std::net::SocketAddr {
    upstream(|req: Request<hyper::body::Incoming>| async move {
        let h = req.headers();
        let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("-");
        let text = format!(
            "{} {}\nhost={}\nxff={}\nproto={}\nxhost={}\nkeepalive={}\nsecret={}",
            req.method(),
            req.uri(),
            get("host"),
            get("x-forwarded-for"),
            get("x-forwarded-proto"),
            get("x-forwarded-host"),
            get("keep-alive"),
            get("x-secret"),
        );
        Response::builder()
            .header("x-upstream", "yes")
            .header("connection", "x-drop")
            .header("x-drop", "1")
            .body(body(text))
            .unwrap()
    })
    .await
}

#[tokio::test]
async fn unmatched_request_gets_404_from_proxlet() {
    let p = Proxlet::start(&config(&[], Path::new("/"))).await;
    let got = get(p.addr, "example.com", "/nothing").await;
    assert_eq!(got.status, StatusCode::NOT_FOUND);
    assert_eq!(got.headers["server"], "proxlet");
    assert!(
        got.body
            .contains("proxlet: no rule for example.com/nothing"),
        "{}",
        got.body
    );
}

#[tokio::test]
async fn path_and_query_are_preserved() {
    let up = echo_upstream().await;
    let p = Proxlet::start(&config(&[("/api", &up.port().to_string())], Path::new("/"))).await;
    let got = get(p.addr, "example.com", "/api/users?page=2").await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(
        got.body.starts_with("GET /api/users?page=2\n"),
        "{}",
        got.body
    );
    assert_eq!(got.headers["x-upstream"], "yes");
}

#[tokio::test]
async fn forwarding_headers_and_hop_by_hop() {
    let up = echo_upstream().await;
    let p = Proxlet::start(&config(&[("/api", &up.port().to_string())], Path::new("/"))).await;
    let got = send(
        p.addr,
        Request::get("/api")
            .header("host", "example.com")
            .header("connection", "keep-alive, x-secret")
            .header("keep-alive", "timeout=5")
            .header("x-secret", "s3cr3t")
            .body(Empty::new())
            .unwrap(),
    )
    .await;
    let b = &got.body;
    assert!(b.contains("host=example.com\n"), "{b}");
    assert!(b.contains("xff=127.0.0.1\n"), "{b}");
    assert!(b.contains("proto=http\n"), "{b}");
    assert!(b.contains("xhost=example.com\n"), "{b}");
    assert!(b.contains("keepalive=-\n"), "{b}");
    assert!(b.ends_with("secret=-"), "{b}");
    // Hop-by-hop headers from the upstream do not reach the client either.
    assert!(!got.headers.contains_key("x-drop"));
}

fn stream_from<T: Send + 'static>(
    mut rx: tokio::sync::mpsc::Receiver<T>,
) -> impl futures_util::Stream<Item = T> + Send + Sync + 'static {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

#[tokio::test]
async fn streaming_response_is_not_buffered() {
    let up = upstream(|_req| async {
        let (tx, rx) =
            tokio::sync::mpsc::channel::<Result<Frame<Bytes>, std::convert::Infallible>>(4);
        tokio::spawn(async move {
            for i in 0..3 {
                let _ = tx
                    .send(Ok(Frame::data(Bytes::from(format!("chunk{i}\n")))))
                    .await;
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        });
        Response::new(BodyExt::boxed(StreamBody::new(stream_from(rx))))
    })
    .await;
    let p = Proxlet::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;

    let stream = tokio::net::TcpStream::connect(p.addr).await.unwrap();
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(conn);
    let started = Instant::now();
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
    let first = BodyExt::frame(&mut body)
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(&first[..], b"chunk0\n");
    assert!(
        started.elapsed() < Duration::from_millis(350),
        "first chunk took {:?}; the response was buffered",
        started.elapsed()
    );
    let remaining = BodyExt::collect(body).await.unwrap().to_bytes();
    assert_eq!(&remaining[..], b"chunk1\nchunk2\n");
}

#[tokio::test]
async fn upstream_down_is_502_naming_route_and_upstream() {
    let port = closed_port();
    let p = Proxlet::start(&config(&[("/api", &port.to_string())], Path::new("/"))).await;
    let got = get(p.addr, "x", "/api").await;
    assert_eq!(got.status, StatusCode::BAD_GATEWAY);
    assert!(
        got.body
            .contains(&format!("/api could not reach 127.0.0.1:{port}")),
        "{}",
        got.body
    );
}

#[tokio::test]
async fn stalled_upstream_is_504() {
    let up = upstream(|_req| async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Response::new(body(""))
    })
    .await;
    let mut cfg = config(&[("/slow", &up.port().to_string())], Path::new("/"));
    cfg.settings.proxy_timeout = Duration::from_millis(300);
    let p = Proxlet::start(&cfg).await;
    let started = Instant::now();
    let got = get(p.addr, "x", "/slow").await;
    assert_eq!(got.status, StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn websocket_through_proxlet() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    if msg.is_text() {
                        let reply = format!("echo: {}", msg.to_text().unwrap());
                        ws.send(Message::text(reply)).await.unwrap();
                    }
                }
            });
        }
    });
    let p = Proxlet::start(&config(&[("/ws", &ws_port.to_string())], Path::new("/"))).await;

    let (mut ws, resp) = tokio_tungstenite::connect_async(format!("ws://{}/ws", p.addr))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    for word in ["hello", "world"] {
        ws.send(Message::text(word)).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(reply.to_text().unwrap(), format!("echo: {word}"));
    }
}

#[tokio::test]
async fn rules_swap_mid_traffic_without_failed_requests() {
    let a = upstream(|_r| async { Response::new(body("a")) }).await;
    let b = upstream(|_r| async { Response::new(body("b")) }).await;
    let cfg_a = config(&[("/", &a.port().to_string())], Path::new("/"));
    let cfg_b = config(&[("/", &b.port().to_string())], Path::new("/"));
    let p = Arc::new(Proxlet::start(&cfg_a).await);

    let failures = Arc::new(AtomicUsize::new(0));
    let seen_b = Arc::new(AtomicUsize::new(0));
    let mut clients = Vec::new();
    for _ in 0..8 {
        let (p, failures, seen_b) = (p.clone(), failures.clone(), seen_b.clone());
        clients.push(tokio::spawn(async move {
            for _ in 0..40 {
                let got = get(p.addr, "x", "/").await;
                match (got.status, got.body.as_str()) {
                    (StatusCode::OK, "a") => {}
                    (StatusCode::OK, "b") => {
                        seen_b.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    for i in 0..20 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        p.replace(if i % 2 == 0 { &cfg_b } else { &cfg_a });
    }
    p.replace(&cfg_b);
    for c in clients {
        c.await.unwrap();
    }
    assert_eq!(failures.load(Ordering::Relaxed), 0);
    assert!(seen_b.load(Ordering::Relaxed) > 0);
    assert_eq!(get(p.addr, "x", "/").await.body, "b");
}
