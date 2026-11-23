//! Graceful shutdown, end to end.
#![allow(missing_docs)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use http::{Response, StatusCode};

#[tokio::test]
async fn in_flight_request_completes_after_shutdown_signal() {
    let up = upstream(|_r| async {
        tokio::time::sleep(Duration::from_millis(600)).await;
        Response::new(body("done"))
    })
    .await;
    let mut p = Proxlet::start(&config(&[("/", &up.port().to_string())], Path::new("/"))).await;
    let addr = p.addr;
    let in_flight = tokio::spawn(async move { get(addr, "x", "/").await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let started = Instant::now();
    let serve = p.shutdown();
    let got = in_flight.await.unwrap();
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body, "done");
    tokio::time::timeout(Duration::from_secs(5), serve)
        .await
        .expect("serve returned after the in-flight request finished")
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));

    // The listener is closed: new connections are refused.
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn grace_period_bounds_shutdown() {
    let up = upstream(|_r| async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Response::new(body("never"))
    })
    .await;
    let mut p = Proxlet::start_with_grace(
        &config(&[("/", &up.port().to_string())], Path::new("/")),
        Duration::from_millis(300),
    )
    .await;
    let addr = p.addr;
    let _stuck = tokio::spawn(async move { get(addr, "x", "/").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    p.shutdown().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
}
