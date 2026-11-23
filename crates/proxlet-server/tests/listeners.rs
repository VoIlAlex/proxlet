//! Two listeners on one engine: releasing the public one, and trusting the front one.
#![allow(missing_docs)]

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use http::{Request, Response, StatusCode};
use http_body_util::Empty;
use proxlet_server::{Connections, Engine, Origin, Snapshot};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

async fn listener() -> (TcpListener, std::net::SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

#[tokio::test]
async fn releasing_a_listener_keeps_its_in_flight_requests() {
    let up = upstream(|_r| async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Response::new(body("done"))
    })
    .await;
    let engine = Engine::new(
        Snapshot::build(&config(&[("/", &up.port().to_string())], Path::new("/"))).unwrap(),
    );
    let conns = Connections::new();
    let (public, public_addr) = listener().await;
    let (release, released) = oneshot::channel::<()>();
    let accept = tokio::spawn(proxlet_server::accept(
        public,
        engine.clone(),
        conns.clone(),
        Origin::Direct,
        async {
            let _ = released.await;
        },
    ));

    let in_flight = tokio::spawn(async move { get(public_addr, "x", "/").await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    release.send(()).unwrap();
    accept.await.unwrap();

    // The port is free again: a new listener can take it straight away.
    let rebound = std::net::TcpListener::bind(public_addr);
    assert!(rebound.is_ok(), "port not released: {rebound:?}");

    let got = in_flight.await.unwrap();
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body, "done");
    conns.drain(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn front_listener_keeps_forwarded_headers() {
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let record = seen.clone();
    let up = upstream(move |r: Request<_>| {
        let record = record.clone();
        async move {
            let h = |k: &str| {
                r.headers()
                    .get(k)
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default()
            };
            record
                .lock()
                .unwrap()
                .push((h("x-forwarded-for"), h("x-forwarded-proto")));
            Response::new(body("ok"))
        }
    })
    .await;
    let engine = Engine::new(
        Snapshot::build(&config(&[("/", &up.port().to_string())], Path::new("/"))).unwrap(),
    );
    let conns = Connections::new();
    let mut stops = Vec::new();
    let mut addrs = Vec::new();
    for origin in [Origin::Direct, Origin::Front] {
        let (l, a) = listener().await;
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(proxlet_server::accept(
            l,
            engine.clone(),
            conns.clone(),
            origin,
            async {
                let _ = rx.await;
            },
        ));
        stops.push(tx);
        addrs.push(a);
    }

    for addr in &addrs {
        let req = Request::get("/")
            .header("host", "site.example")
            .header("x-forwarded-for", "203.0.113.7")
            .header("x-forwarded-proto", "https")
            .body(Empty::new())
            .unwrap();
        assert_eq!(send(*addr, req).await.status, StatusCode::OK);
    }
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            ("203.0.113.7, 127.0.0.1".to_owned(), "http".to_owned()),
            ("203.0.113.7".to_owned(), "https".to_owned()),
        ]
    );
    drop(stops);
}
