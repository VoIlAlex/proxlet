//! The proxlet engine: listeners, reverse proxying and static file serving.
//!
//! The engine serves from an immutable [`Snapshot`] held in an atomic pointer.
//! Each request loads the current snapshot once and keeps it for its lifetime,
//! so replacing the snapshot never affects in-flight requests and the request
//! path takes no lock.

mod files;
mod proxy;
mod response;
mod snapshot;

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use bytes::Bytes;
use http::{Request, Response, header};
use http_body_util::combinators::BoxBody;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;

pub use snapshot::{BuildError, Snapshot};

/// Error type carried by response bodies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The body type of every response the engine produces.
pub type Body = BoxBody<Bytes, BoxError>;

struct ClientSlot {
    connect_timeout: Duration,
    client: proxy::UpstreamClient,
}

/// Shared engine state: the current snapshot and the upstream client.
pub struct Engine {
    snapshot: ArcSwap<Snapshot>,
    client: ArcSwap<ClientSlot>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("snapshot", &self.snapshot.load())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Creates an engine serving `snapshot`.
    #[must_use]
    pub fn new(snapshot: Snapshot) -> Arc<Self> {
        let connect_timeout = snapshot.settings.connect_timeout;
        Arc::new(Self {
            snapshot: ArcSwap::from_pointee(snapshot),
            client: ArcSwap::from_pointee(ClientSlot {
                connect_timeout,
                client: proxy::client(connect_timeout),
            }),
        })
    }

    /// Atomically replaces the snapshot. In-flight requests finish on the old one.
    pub fn replace(&self, snapshot: Snapshot) {
        let connect_timeout = snapshot.settings.connect_timeout;
        if self.client.load().connect_timeout != connect_timeout {
            self.client.store(Arc::new(ClientSlot {
                connect_timeout,
                client: proxy::client(connect_timeout),
            }));
        }
        self.snapshot.store(Arc::new(snapshot));
    }

    /// The snapshot currently being served.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.load_full()
    }

    /// Handles one request from a client that connected directly.
    pub async fn handle(&self, peer: SocketAddr, req: Request<Incoming>) -> Response<Body> {
        self.handle_from(peer, Origin::Direct, req).await
    }

    /// Handles one request that arrived through `origin`.
    pub async fn handle_from(
        &self,
        peer: SocketAddr,
        origin: Origin,
        req: Request<Incoming>,
    ) -> Response<Body> {
        let snap = self.snapshot.load_full();
        let host = req
            .uri()
            .authority()
            .map(|a| a.as_str().to_owned())
            .or_else(|| {
                req.headers()
                    .get(header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            });
        let Some(m) = snap.table.lookup(host.as_deref(), req.uri().path()) else {
            return response::no_rule(host.as_deref(), req.uri().path());
        };
        match m.value {
            snapshot::Handler::Proxy(target) => {
                let client = self.client.load().client.clone();
                let timeout = snap.settings.proxy_timeout;
                proxy::forward(client, timeout, target, peer, origin, host, req).await
            }
            snapshot::Handler::Files(target) => {
                files::serve(&snap.settings, target, m.prefix, &req).await
            }
        }
    }
}

/// Where a connection came from, which decides how forwarding headers are set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A client connected to proxlet directly.
    Direct,
    /// A trusted front server (nginx) on loopback forwarded the request; its
    /// `X-Forwarded-For` and `X-Forwarded-Proto` are kept as they are.
    Front,
}

/// The open connections of every listener, so they can be drained together.
///
/// Cloning shares the set. Stopping one listener's accept loop leaves its
/// connections running; [`Connections::drain`] ends them all.
#[derive(Clone)]
pub struct Connections {
    graceful: Arc<GracefulShutdown>,
}

impl std::fmt::Debug for Connections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connections").finish_non_exhaustive()
    }
}

impl Default for Connections {
    fn default() -> Self {
        Self::new()
    }
}

impl Connections {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graceful: Arc::new(GracefulShutdown::new()),
        }
    }

    /// Asks every open connection to finish its current request and close,
    /// waiting up to `grace`. Call it after every accept loop has returned.
    pub async fn drain(self, grace: Duration) {
        let Some(graceful) = Arc::into_inner(self.graceful) else {
            tracing::warn!("an accept loop is still running; not waiting for connections");
            return;
        };
        tokio::select! {
            () = graceful.shutdown() => {}
            () = tokio::time::sleep(grace) => {
                tracing::warn!("grace period of {}s ended with connections still open", grace.as_secs());
            }
        }
    }
}

/// Accepts connections on `listener` until `stop` resolves, then closes the
/// listener and returns. Accepted connections keep running in `conns`.
pub async fn accept(
    listener: TcpListener,
    engine: Arc<Engine>,
    conns: Connections,
    origin: Origin,
    stop: impl Future<Output = ()>,
) {
    tokio::pin!(stop);
    loop {
        let (stream, peer) = tokio::select! {
            () = &mut stop => break,
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    // Usually EMFILE; back off briefly instead of spinning.
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        let _ = stream.set_nodelay(true);
        let engine = engine.clone();
        let service = service_fn(move |req| {
            let engine = engine.clone();
            async move { Ok::<_, Infallible>(engine.handle_from(peer, origin, req).await) }
        });
        let conn = auto::Builder::new(TokioExecutor::new())
            .serve_connection_with_upgrades(TokioIo::new(stream), service)
            .into_owned();
        let conn = conns.graceful.watch(conn);
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(%peer, "connection ended with error: {e}");
            }
        });
    }
    drop(listener);
}

/// Accepts connections on `listener` and serves them until `shutdown`
/// resolves. Then it stops accepting, gives open connections up to `grace` to
/// finish their requests, and returns.
pub async fn serve(
    listener: TcpListener,
    engine: Arc<Engine>,
    shutdown: impl Future<Output = ()>,
    grace: Duration,
) {
    let conns = Connections::new();
    accept(listener, engine, conns.clone(), Origin::Direct, shutdown).await;
    conns.drain(grace).await;
}
