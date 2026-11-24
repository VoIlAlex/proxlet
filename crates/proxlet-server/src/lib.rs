//! The proxlet engine: listeners, reverse proxying and static file serving.
//!
//! The engine serves from an immutable [`Snapshot`] held in an atomic pointer.
//! Each request loads the current snapshot once and keeps it for its lifetime,
//! so replacing the snapshot never affects in-flight requests and the request
//! path takes no lock.

mod access;
mod files;
mod proxy;
mod response;
mod snapshot;
mod stats;
pub mod tls;

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::combinators::BoxBody;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

pub use access::{AccessLog, AccessRecord};
pub use snapshot::{BuildError, Snapshot, StatsRow};
use stats::InFlight;
pub use stats::{RuleStats, Totals};

/// Error type carried by response bodies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The body type of every response the engine produces.
pub type Body = BoxBody<Bytes, BoxError>;

struct ClientSlot {
    connect_timeout: Duration,
    client: proxy::UpstreamClient,
}

/// The path prefix of HTTP-01 challenge requests.
pub const CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

/// What the redirect to HTTPS needs to know (design decision 4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsView {
    /// Lowercase names that have a certificate.
    pub names: HashSet<String>,
    /// Whether HTTPS is being served right now, by proxlet or by a front server.
    pub https_served: bool,
    /// The public HTTPS port, for the redirect's `Location`.
    pub https_port: u16,
}

/// Shared engine state: the current snapshot, the upstream client, pending
/// ACME challenges and what the HTTPS redirect needs.
pub struct Engine {
    snapshot: ArcSwap<Snapshot>,
    client: ArcSwap<ClientSlot>,
    challenges: ArcSwap<HashMap<String, String>>,
    tls: ArcSwap<TlsView>,
    access_log: ArcSwap<Option<Arc<dyn AccessLog>>>,
    connections: AtomicU64,
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
            challenges: ArcSwap::from_pointee(HashMap::new()),
            tls: ArcSwap::from_pointee(TlsView::default()),
            access_log: ArcSwap::from_pointee(None),
            connections: AtomicU64::new(0),
        })
    }

    /// Starts answering the HTTP-01 challenge `token` with `key_authorization`.
    pub fn publish_challenge(&self, token: &str, key_authorization: &str) {
        self.challenges.rcu(|m| {
            let mut m = HashMap::clone(m);
            m.insert(token.to_owned(), key_authorization.to_owned());
            m
        });
    }

    /// Stops answering the challenge `token`.
    pub fn withdraw_challenge(&self, token: &str) {
        self.challenges.rcu(|m| {
            let mut m = HashMap::clone(m);
            m.remove(token);
            m
        });
    }

    /// Replaces what the HTTPS redirect is based on.
    pub fn set_tls_view(&self, view: TlsView) {
        self.tls.store(Arc::new(view));
    }

    /// What the HTTPS redirect is currently based on.
    #[must_use]
    pub fn tls_view(&self) -> Arc<TlsView> {
        self.tls.load_full()
    }

    /// Sends a record of every request answered from now on to `log`, while
    /// the `log.access` setting is on.
    pub fn set_access_log(&self, log: Arc<dyn AccessLog>) {
        self.access_log.store(Arc::new(Some(log)));
    }

    /// Every rule's live statistics, the "no rule" row last, and the number
    /// of open client connections.
    #[must_use]
    pub fn stats(&self) -> StatsSnapshot {
        StatsSnapshot {
            connections: self.connections.load(Ordering::Relaxed),
            rows: self.snapshot.load().stats(),
        }
    }

    /// Atomically replaces the snapshot. In-flight requests finish on the old one.
    pub fn replace(&self, mut snapshot: Snapshot) {
        snapshot.adopt_stats(&self.snapshot.load());
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
        let start = Instant::now();
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
        let sink = if snap.settings.log_access {
            Option::clone(&self.access_log.load())
        } else {
            None
        };
        let pending =
            sink.map(|sink| access::Pending::start(sink, peer, origin, host.as_deref(), &req));
        let (resp, mut served) = self.respond(&snap, peer, origin, host, req).await;
        let in_flight = served
            .in_flight
            .take()
            .unwrap_or_else(|| InFlight::start(Arc::clone(&snap.unmatched)));
        in_flight.0.record(
            resp.status().as_u16(),
            access::upstream_failed(&resp),
            start.elapsed(),
        );
        access::wrap(resp, served, in_flight, pending)
    }

    async fn respond(
        &self,
        snap: &Snapshot,
        peer: SocketAddr,
        origin: Origin,
        host: Option<String>,
        req: Request<Incoming>,
    ) -> (Response<Body>, access::Served) {
        if let Some(resp) = self.challenge_response(&req) {
            return (resp, access::Served::other("acme-challenge"));
        }
        if let Some(resp) = self.redirect(snap, origin, host.as_deref(), &req) {
            return (resp, access::Served::other("redirect"));
        }
        let Some(m) = snap.table.lookup(host.as_deref(), req.uri().path()) else {
            let resp = response::no_rule(host.as_deref(), req.uri().path());
            return (resp, access::Served::default());
        };
        match m.value {
            snapshot::Handler::Proxy(target) => {
                let served = access::Served {
                    rule: Some(target.route.clone()),
                    target: Some(target.authority.to_string()),
                    in_flight: Some(InFlight::start(Arc::clone(&target.stats))),
                };
                let client = self.client.load().client.clone();
                let timeout = snap.settings.proxy_timeout;
                let resp = proxy::forward(client, timeout, target, peer, origin, host, req).await;
                (resp, served)
            }
            snapshot::Handler::Files(target) => {
                let served = access::Served {
                    rule: Some(target.route.clone()),
                    target: Some(target.root.display().to_string()),
                    in_flight: Some(InFlight::start(Arc::clone(&target.stats))),
                };
                (
                    files::serve(&snap.settings, target, m.prefix, &req).await,
                    served,
                )
            }
        }
    }
}

impl Engine {
    fn challenge_response(&self, req: &Request<Incoming>) -> Option<Response<Body>> {
        if !matches!(*req.method(), Method::GET | Method::HEAD) {
            return None;
        }
        let token = req.uri().path().strip_prefix(CHALLENGE_PREFIX)?;
        let key_auth = self.challenges.load().get(token).cloned()?;
        let mut resp = Response::new(response::full(key_auth));
        resp.headers_mut().insert(header::SERVER, response::SERVER);
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        Some(resp)
    }

    fn redirect(
        &self,
        snap: &Snapshot,
        origin: Origin,
        host: Option<&str>,
        req: &Request<Incoming>,
    ) -> Option<Response<Body>> {
        if !snap.settings.tls_redirect || req.uri().path().starts_with(CHALLENGE_PREFIX) {
            return None;
        }
        let view = self.tls.load();
        if !view.https_served || effective_scheme(origin, req) != "http" {
            return None;
        }
        let name = host_name(host?).to_ascii_lowercase();
        if !view.names.contains(&name) {
            return None;
        }
        let port = if view.https_port == 443 {
            String::new()
        } else {
            format!(":{}", view.https_port)
        };
        let path = req
            .uri()
            .path_and_query()
            .map_or("/", http::uri::PathAndQuery::as_str);
        let location = HeaderValue::from_str(&format!("https://{name}{port}{path}")).ok()?;
        let mut resp = response::status(StatusCode::PERMANENT_REDIRECT);
        resp.headers_mut().insert(header::LOCATION, location);
        Some(resp)
    }
}

/// `http` or `https`: how the client reached proxlet (or the front server).
fn effective_scheme(origin: Origin, req: &Request<Incoming>) -> &str {
    match origin {
        Origin::Tls => "https",
        Origin::Direct => "http",
        Origin::Front => req
            .headers()
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map_or("http", str::trim),
    }
}

/// The host without a port: `example.com:8080` → `example.com`, `[::1]:80` → `[::1]`.
fn host_name(host: &str) -> &str {
    if host.starts_with('[') {
        return host.split_inclusive(']').next().unwrap_or(host);
    }
    host.rsplit_once(':').map_or(host, |(h, _)| h)
}

/// The engine's live statistics at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct StatsSnapshot {
    /// Open client connections, on every listener.
    pub connections: u64,
    /// One row per rule in `proxlet ls` order, then the "no rule" row.
    pub rows: Vec<StatsRow>,
}

/// Counts one open client connection until dropped.
struct OpenConnection(Arc<Engine>);

impl OpenConnection {
    fn new(engine: Arc<Engine>) -> Self {
        engine.connections.fetch_add(1, Ordering::Relaxed);
        Self(engine)
    }
}

impl Drop for OpenConnection {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
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
    /// A client connected to proxlet's HTTPS listener.
    Tls,
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

/// How long a client may take to finish the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Accepts connections on `listener` until `stop` resolves, then closes the
/// listener and returns. Accepted connections keep running in `conns`.
pub async fn accept(
    listener: TcpListener,
    engine: Arc<Engine>,
    conns: Connections,
    origin: Origin,
    stop: impl Future<Output = ()>,
) {
    accept_with(listener, engine, conns, origin, None, stop).await;
}

/// Like [`accept`] for the HTTPS listener: each connection's TLS handshake
/// runs in its own task (bounded by [`HANDSHAKE_TIMEOUT`]), so a slow client
/// never holds up the accept loop.
pub async fn accept_tls(
    listener: TcpListener,
    engine: Arc<Engine>,
    conns: Connections,
    tls: Arc<rustls::ServerConfig>,
    stop: impl Future<Output = ()>,
) {
    let acceptor = TlsAcceptor::from(tls);
    accept_with(listener, engine, conns, Origin::Tls, Some(acceptor), stop).await;
}

async fn accept_with(
    listener: TcpListener,
    engine: Arc<Engine>,
    conns: Connections,
    origin: Origin,
    tls: Option<TlsAcceptor>,
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
        let watcher = conns.graceful.watcher();
        let tls = tls.clone();
        tokio::spawn(async move {
            match tls {
                None => serve_io(TokioIo::new(stream), engine, peer, origin, watcher).await,
                Some(acceptor) => {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls_stream)) => {
                            serve_io(TokioIo::new(tls_stream), engine, peer, origin, watcher).await;
                        }
                        Ok(Err(e)) => tracing::debug!(%peer, "TLS handshake failed: {e}"),
                        Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
                    }
                }
            }
        });
    }
    drop(listener);
}

async fn serve_io<IO>(
    io: TokioIo<IO>,
    engine: Arc<Engine>,
    peer: SocketAddr,
    origin: Origin,
    watcher: hyper_util::server::graceful::Watcher,
) where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let _open = OpenConnection::new(Arc::clone(&engine));
    let service = service_fn(move |req| {
        let engine = engine.clone();
        async move { Ok::<_, Infallible>(engine.handle_from(peer, origin, req).await) }
    });
    let conn = auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(io, service)
        .into_owned();
    if let Err(e) = watcher.watch(conn).await {
        tracing::debug!(%peer, "connection ended with error: {e}");
    }
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
