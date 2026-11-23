//! Shared helpers for engine integration tests.
#![allow(dead_code, missing_docs)]

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use proxlet_core::{Config, Rule, Target};
use proxlet_server::{Engine, Snapshot};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

pub type TestBody = BoxBody<Bytes, Infallible>;

pub fn body(s: impl Into<Bytes>) -> TestBody {
    http_body_util::Full::new(s.into()).boxed()
}

/// Builds a config from `(route, target)` pairs; relative directories resolve against `cwd`.
pub fn config(rules: &[(&str, &str)], cwd: &Path) -> Config {
    let mut c = Config::default();
    for (route, target) in rules {
        c.rules.push(Rule {
            route: route.parse().unwrap(),
            target: Target::parse(target, cwd).unwrap().validate().unwrap(),
        });
    }
    c
}

pub struct Proxlet {
    pub addr: SocketAddr,
    pub engine: Arc<Engine>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Proxlet {
    pub async fn start(config: &Config) -> Self {
        Self::start_with_grace(config, Duration::from_secs(5)).await
    }

    pub async fn start_with_grace(config: &Config, grace: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Engine::new(Snapshot::build(config).unwrap());
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(proxlet_server::serve(
            listener,
            engine.clone(),
            async {
                let _ = rx.await;
            },
            grace,
        ));
        Self {
            addr,
            engine,
            shutdown: Some(tx),
            task: Some(task),
        }
    }

    pub fn replace(&self, config: &Config) {
        self.engine.replace(Snapshot::build(config).unwrap());
    }

    /// Signals shutdown and returns the serve task to await.
    pub fn shutdown(&mut self) -> tokio::task::JoinHandle<()> {
        let _ = self.shutdown.take().unwrap().send(());
        self.task.take().unwrap()
    }
}

/// Starts an HTTP/1.1 upstream on a random port that answers with `handler`.
pub async fn upstream<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<TestBody>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let handler = handler.clone();
                    async move { Ok::<_, Infallible>(handler(req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .with_upgrades()
                    .await;
            });
        }
    });
    addr
}

/// A port nothing listens on.
pub fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

pub struct Got {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

/// Sends one request over a fresh HTTP/1.1 connection and collects the response.
pub async fn send(addr: SocketAddr, req: Request<Empty<Bytes>>) -> Got {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    Got {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// GET `path` with the given `Host`.
pub async fn get(addr: SocketAddr, host: &str, path: &str) -> Got {
    send(
        addr,
        Request::get(path)
            .header("host", host)
            .body(Empty::new())
            .unwrap(),
    )
    .await
}

/// Writes raw bytes and reads until the server closes the connection.
pub async fn raw(addr: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}
