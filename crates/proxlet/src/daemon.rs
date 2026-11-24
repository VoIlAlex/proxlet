//! The server process: `proxlet run`.

use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proxlet_core::{ConfigFile, Locations, port};
use proxlet_front::FrontServer;
use proxlet_server::tls::{CertResolver, server_config};
use proxlet_server::{Connections, Engine, Origin, Snapshot};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc, watch};

use crate::attach;
use crate::certs::{self, Certs};
use crate::control::{self, CertReply, RenewReply, Reply, Request, StatusInfo, WireOutcome};
use crate::output::{CmdResult, Failure};
use crate::pipeline;
use crate::ports::{self, Mode};

/// How long in-flight requests get to finish after a stop signal.
pub const GRACE: Duration = Duration::from_secs(10);

/// Environment variable the system service sets to the manager's name.
pub const SERVICE_ENV: &str = "PROXLET_SERVICE";

struct State {
    engine: Arc<Engine>,
    file: ConfigFile,
    apply_lock: Mutex<()>,
    started: Instant,
    listen: SocketAddr,
    https_listen: SocketAddr,
    internal: SocketAddr,
    mode: watch::Receiver<Mode>,
    https_mode: watch::Receiver<Mode>,
    ports: mpsc::Sender<ports::Command>,
    certs: Arc<Certs>,
    run_by: String,
    stop: watch::Sender<bool>,
}

/// Runs the server in the foreground until interrupted.
pub fn run(locations: &Locations) -> CmdResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_async(locations))
}

async fn run_async(locations: &Locations) -> CmdResult {
    let socket = locations.socket();
    if control::running(&socket).is_some() {
        return Err(Failure::new("proxlet is already running").hint("See it with: proxlet status"));
    }

    let file = ConfigFile::new(locations.config_file());
    let (_, config) = file.read()?;
    let snapshot = Snapshot::build(&config).map_err(|e| Failure::new(e.to_string()))?;
    let settings = &config.settings;
    let listen = SocketAddr::new(settings.listen_address, settings.http_port);
    let https_listen = SocketAddr::new(settings.listen_address, settings.https_port);
    let internal = SocketAddr::from(([127, 0, 0, 1], settings.attach_port));

    let internal_listener = bind_internal(internal)?;
    let foreground = std::env::var_os(SERVICE_ENV).is_none();
    let front: Arc<dyn FrontServer> = Arc::from(attach::system_front());
    let ledger = locations.state_dir.join(attach::LEDGER);
    let attached_here = foreground && attach_in_foreground(&*front, &ledger, settings);
    let first = bind_public(listen)?;
    let first_https = bind_https(https_listen)?;
    let control_listener = bind_control(locations)?;

    let rules = snapshot.rule_count();
    let engine = Engine::new(snapshot);
    let conns = Connections::new();
    let (stop, stop_rx) = watch::channel(false);
    let wiring = wire_ports();
    let resolver = CertResolver::new();
    let certs = Arc::new(Certs::new(
        &locations.state_dir,
        resolver.clone(),
        engine.clone(),
        wiring.https_mode.clone(),
        https_listen.port(),
        Box::new(|| false),
    ));
    let cert_count = certs.reload();
    let state = Arc::new(State {
        engine: engine.clone(),
        file,
        apply_lock: Mutex::new(()),
        started: Instant::now(),
        listen,
        https_listen,
        internal,
        mode: wiring.mode,
        https_mode: wiring.https_mode,
        ports: wiring.ports,
        certs: certs.clone(),
        run_by: std::env::var(SERVICE_ENV).unwrap_or_else(|_| "foreground".into()),
        stop,
    });

    tokio::spawn(control_loop(control_listener, state.clone()));
    log_start(
        rules,
        &state,
        first.is_some(),
        first_https.is_some(),
        cert_count,
    );

    let internal_task = tokio::spawn(proxlet_server::accept(
        internal_listener,
        engine.clone(),
        conns.clone(),
        Origin::Front,
        stopped(stop_rx.clone()),
    ));
    spawn_watcher(&front, &engine, &state, &stop_rx);
    let probe: Arc<dyn ports::Probe> = Arc::new(HostProbe {
        front: front.clone(),
    });
    let owner = public_owner(listen, probe.clone(), &engine, &conns, None);
    let owner_task = tokio::spawn(owner.run(
        first,
        wiring.mode_tx,
        wiring.http_rx,
        stopped(stop_rx.clone()),
    ));
    let tls = Some(server_config(resolver));
    let https_owner = public_owner(https_listen, probe, &engine, &conns, tls);
    let https_task = tokio::spawn(https_owner.run(
        first_https,
        wiring.https_mode_tx,
        wiring.https_rx,
        stopped(stop_rx.clone()),
    ));
    spawn_cert_tasks(certs, &state, &stop_rx);

    wait_for_stop(stop_rx).await?;
    stop_serving(&state, [internal_task, owner_task, https_task], conns).await;
    if attached_here {
        detach_in_foreground(&*front, &ledger);
    }
    let _ = std::fs::remove_file(&socket);
    tracing::info!("stopped");
    Ok(())
}

/// Stops every listener, then gives open connections [`GRACE`] to finish.
async fn stop_serving(
    state: &State,
    listeners: [tokio::task::JoinHandle<()>; 3],
    conns: Connections,
) {
    tracing::info!(
        "stopping; waiting up to {}s for in-flight requests",
        GRACE.as_secs()
    );
    let _ = state.stop.send(true);
    for task in listeners {
        let _ = task.await;
    }
    conns.drain(GRACE).await;
}

/// The channels between the control socket, the two port owners and status.
struct PortWiring {
    mode_tx: watch::Sender<Mode>,
    mode: watch::Receiver<Mode>,
    https_mode_tx: watch::Sender<Mode>,
    https_mode: watch::Receiver<Mode>,
    http_rx: mpsc::Receiver<ports::Command>,
    https_rx: mpsc::Receiver<ports::Command>,
    /// Commands for both owners (see [`ports::fan_out`]).
    ports: mpsc::Sender<ports::Command>,
}

fn wire_ports() -> PortWiring {
    let checking = || Mode::Waiting {
        reason: ports::CHECKING.into(),
    };
    let (mode_tx, mode) = watch::channel(checking());
    let (https_mode_tx, https_mode) = watch::channel(checking());
    let (http_tx, http_rx) = mpsc::channel(8);
    let (https_tx, https_rx) = mpsc::channel(8);
    let (ports, ports_rx) = mpsc::channel(8);
    tokio::spawn(ports::fan_out(ports_rx, vec![http_tx, https_tx]));
    PortWiring {
        mode_tx,
        mode,
        https_mode_tx,
        https_mode,
        http_rx,
        https_rx,
        ports,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn log_start(rules: usize, state: &State, http: bool, https: bool, certs: usize) {
    tracing::info!(
        "proxlet {} started with {rules} rule{}; internal listener on http://{}",
        env!("CARGO_PKG_VERSION"),
        plural(rules),
        state.internal
    );
    if http {
        tracing::info!("listening on http://{}", state.listen);
    }
    if https {
        tracing::info!(
            "listening on https://{} with {certs} certificate{}",
            state.https_listen,
            plural(certs)
        );
    }
}

/// Waits for Ctrl-C, SIGTERM or a `shutdown` request.
async fn wait_for_stop(mut stop: watch::Receiver<bool>) -> std::io::Result<()> {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = stop.changed() => {}
    }
    Ok(())
}

/// The owner of a public port, serving TLS when `tls` is set.
fn public_owner(
    addr: SocketAddr,
    probe: Arc<dyn ports::Probe>,
    engine: &Arc<Engine>,
    conns: &Connections,
    tls: Option<Arc<proxlet_server::tls::ServerConfig>>,
) -> ports::Owner {
    ports::Owner {
        binder: Box::new(move || std::net::TcpListener::bind(addr)),
        probe,
        engine: engine.clone(),
        conns: conns.clone(),
        port: addr.port(),
        retry: ports::RETRY,
        hold: ports::HOLD,
        tls,
    }
}

/// Keeps the redirect view current, and renews certificates when due.
fn spawn_cert_tasks(certs: Arc<Certs>, state: &Arc<State>, stop: &watch::Receiver<bool>) {
    tokio::spawn(follow_https_mode(certs.clone(), state.https_mode.clone()));
    let (first_check, every) = certs::renewal_schedule();
    tokio::spawn(certs::renewal_loop(certs, first_check, every, stop.clone()));
}

/// Keeps the redirect view in step with whether HTTPS is being served.
async fn follow_https_mode(certs: Arc<Certs>, mut mode: watch::Receiver<Mode>) {
    while mode.changed().await.is_ok() {
        certs.refresh_view();
    }
}

/// Follows the front server, if one is installed, to hand it the ports when it starts.
fn spawn_watcher(
    front: &Arc<dyn FrontServer>,
    engine: &Arc<Engine>,
    state: &Arc<State>,
    stop: &watch::Receiver<bool>,
) {
    let Some(inst) = front.detect() else {
        return;
    };
    let log = front
        .read(&inst)
        .ok()
        .and_then(|cfg| front.error_log(&inst, &cfg))
        .or(inst.default_error_log);
    let engine = engine.clone();
    tokio::spawn(crate::watch::run(
        front.clone(),
        log,
        vec![state.listen.port(), state.https_listen.port()],
        move || engine.snapshot().settings().attach_enabled,
        vec![state.mode.clone(), state.https_mode.clone()],
        state.ports.clone(),
        stop.clone(),
    ));
}

async fn stopped(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

fn foreground_attach<'a>(
    front: &'a dyn FrontServer,
    ledger: &Path,
    confirm: &'a dyn Fn(&str) -> Option<bool>,
) -> attach::Attach<'a> {
    attach::Attach {
        front,
        manager: None,
        ledger: ledger.to_owned(),
        owner: None,
        hook: None,
        yes: false,
        confirm,
    }
}

/// `proxlet run` sets up the attach itself only when it may write the front
/// server's config (e.g. Homebrew's, owned by the user). Returns whether it did.
fn attach_in_foreground(
    front: &dyn FrontServer,
    ledger: &Path,
    settings: &proxlet_core::Settings,
) -> bool {
    if !settings.attach_enabled {
        return false;
    }
    let Some(inst) = front.detect() else {
        return false;
    };
    let writable = front.read(&inst).is_ok_and(|cfg| {
        cfg.http_includes
            .first()
            .is_some_and(|d| nix::unistd::access(&d.dir, nix::unistd::AccessFlags::W_OK).is_ok())
    });
    if !writable {
        return false;
    }
    let confirm = crate::output::confirm;
    match foreground_attach(front, ledger, &confirm).attach(settings) {
        Ok(a) => {
            match &a.outcome {
                attach::Outcome::Attached { wrote, .. } => tracing::info!(
                    "attached to {}{}",
                    a.front,
                    wrote
                        .as_ref()
                        .map(|w| format!(" ({})", w.display()))
                        .unwrap_or_default()
                ),
                attach::Outcome::Declined { default_at } => tracing::warn!(
                    "{} keeps its own default server at {default_at}; requests for hosts it does not serve go there",
                    a.front
                ),
                _ => {}
            }
            matches!(
                a.outcome,
                attach::Outcome::Attached { .. }
                    | attach::Outcome::AlreadyAttached
                    | attach::Outcome::Declined { .. }
            )
        }
        Err(e) => {
            tracing::warn!("could not attach to {}: {}", front.name(), e.message);
            false
        }
    }
}

fn detach_in_foreground(front: &dyn FrontServer, ledger: &Path) {
    let confirm = |_: &str| None;
    match foreground_attach(front, ledger, &confirm).detach() {
        Ok(Some(warnings)) => {
            for w in warnings {
                tracing::warn!("{w}");
            }
            tracing::info!("detached from {}", front.name());
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("could not detach from {}: {}", front.name(), e.message),
    }
}

/// Recognises the process holding the public port (design decision 6).
struct HostProbe {
    front: Arc<dyn FrontServer>,
}

impl ports::Probe for HostProbe {
    fn classify(&self, port: u16, attach: bool) -> ports::Holder {
        let holder = port::holder(port);
        if attach {
            let cfg = self
                .front
                .detect()
                .and_then(|inst| self.front.read(&inst).ok());
            if self.front.holds(holder.as_ref(), port, cfg.as_ref()) {
                return ports::Holder::Front(self.front.name());
            }
        }
        ports::Holder::Other(holder)
    }

    fn front_running(&self) -> bool {
        self.front.is_running()
    }
}

/// Binds the public listener. A taken port is not an error: the port owner
/// waits for it. Anything else stops the server before it starts.
fn bind_public(addr: SocketAddr) -> Result<Option<std::net::TcpListener>, Failure> {
    let port = addr.port();
    match std::net::TcpListener::bind(addr) {
        Ok(l) => Ok(Some(l)),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(Failure::new(format!(
            "binding port {port} needs elevated privileges"
        ))
        .hint("Run proxlet as a system service: sudo proxlet start")
        .hint("Or use a high port: proxlet config listen.http_port 8080")),
        Err(e) => Err(Failure::new(format!("cannot listen on {addr}: {e}"))),
    }
}

/// Binds the HTTPS listener. Unlike the HTTP port, a port that needs
/// privileges is not fatal: plain HTTP keeps working and the port owner
/// reports why HTTPS is waiting.
fn bind_https(addr: SocketAddr) -> Result<Option<std::net::TcpListener>, Failure> {
    match std::net::TcpListener::bind(addr) {
        Ok(l) => Ok(Some(l)),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(Failure::new(format!("cannot listen on {addr}: {e}"))),
    }
}

/// Binds the loopback listener a front server forwards to.
fn bind_internal(addr: SocketAddr) -> Result<tokio::net::TcpListener, Failure> {
    let port = addr.port();
    let listener = std::net::TcpListener::bind(addr).map_err(|e| match e.kind() {
        std::io::ErrorKind::AddrInUse => {
            let by = port::holder(port).map_or_else(String::new, |h| format!(" by {h}"));
            Failure::new(format!("the internal port {port} is already in use{by}"))
                .hint("Pick another one: proxlet config attach.port 47081")
        }
        _ => Failure::new(format!("cannot listen on {addr}: {e}")),
    })?;
    listener.set_nonblocking(true)?;
    Ok(tokio::net::TcpListener::from_std(listener)?)
}

fn bind_control(locations: &Locations) -> Result<UnixListener, Failure> {
    let socket = &locations.socket();
    if let Some(dir) = socket.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if locations.socket_is_shortened() {
            ensure_private(dir)?;
        }
    }
    // Nothing answered on the socket, so any file left there is stale.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(|e| {
        Failure::new(format!(
            "cannot create the control socket {}: {e}",
            socket.display()
        ))
    })?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Refuses a shared directory (under `/tmp`) that another user owns or can write to.
fn ensure_private(dir: &Path) -> Result<(), Failure> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(dir)?;
    let mine = meta.uid() == nix::unistd::geteuid().as_raw();
    if !meta.is_dir() || !mine || meta.mode() & 0o077 != 0 {
        return Err(Failure::new(format!(
            "{} is not a private directory owned by you; refusing to put the control socket there",
            dir.display()
        ))
        .hint("Remove it, or set PROXLET_HOME to a shorter path"));
    }
    Ok(())
}

async fn control_loop(listener: UnixListener, state: Arc<State>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle_connection(stream, state.clone()));
            }
            Err(e) => {
                tracing::warn!("control socket accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, state: Arc<State>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle(&state, request).await,
            Err(e) => Reply::error(format!("malformed request: {e}")),
        };
        let mut out = serde_json::to_string(&reply).unwrap_or_else(|_| r#"{"ok":false}"#.into());
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn handle(state: &Arc<State>, request: Request) -> Reply {
    match request {
        Request::Ping => Reply::ok(),
        Request::Status => {
            let (certs, cert_warnings) = state.certs.summary();
            Reply {
                status: Some(StatusInfo {
                    version: env!("CARGO_PKG_VERSION").into(),
                    pid: std::process::id(),
                    mode: state.mode.borrow().label(state.listen.port()),
                    listen: state.listen.to_string(),
                    internal: state.internal.to_string(),
                    run_by: state.run_by.clone(),
                    uptime_secs: state.started.elapsed().as_secs(),
                    rules: state.engine.snapshot().rule_count(),
                    https_mode: state.https_mode.borrow().label(state.https_listen.port()),
                    https_listen: state.https_listen.to_string(),
                    certs,
                    cert_warnings,
                }),
                ..Reply::ok()
            }
        }
        Request::Apply { change } => {
            let change = match change.into_change() {
                Ok(c) => c,
                Err(e) => return Reply::error(e),
            };
            // Serialise applies so snapshots are swapped in the order they were written.
            let _guard = state.apply_lock.lock().await;
            let file = state.file.clone();
            let result = tokio::task::spawn_blocking(move || pipeline::apply(&file, &change)).await;
            match result {
                Ok(Ok((outcome, snapshot))) => {
                    state.engine.replace(snapshot);
                    Reply {
                        outcome: Some(WireOutcome::from(&outcome)),
                        ..Reply::ok()
                    }
                }
                Ok(Err(e)) => Reply::error(e.to_string()),
                Err(e) => Reply::error(format!("internal error: {e}")),
            }
        }
        Request::Shutdown => {
            let _ = state.stop.send(true);
            Reply::ok()
        }
        Request::Release => port_command(state, ports::Command::Release).await,
        Request::Reclaim => port_command(state, ports::Command::Reclaim).await,
        Request::Probe => Reply {
            probe: Some(state.certs.probe()),
            ..Reply::ok()
        },
        Request::Issue {
            domain,
            accept_terms,
        } => match state.certs.issue(&domain, accept_terms).await {
            Ok(stored) => Reply {
                cert: Some(CertReply {
                    domain: stored.domain,
                    not_after: stored.leaf.not_after,
                }),
                ..Reply::ok()
            },
            Err(proxlet_cert::acme::AcmeError::TermsNeeded { url }) => Reply {
                terms: Some(url.unwrap_or_default()),
                ..Reply::error("the ACME server's terms of service must be accepted first")
            },
            Err(e) => Reply::error(e.to_string()),
        },
        Request::Renew { domains } => {
            let renewed = state
                .certs
                .renew(&domains, true)
                .await
                .into_iter()
                .map(|r| match r.result {
                    Ok(s) => RenewReply {
                        domain: r.domain,
                        not_after: Some(s.leaf.not_after),
                        error: None,
                    },
                    Err(e) => RenewReply {
                        domain: r.domain,
                        not_after: None,
                        error: Some(e),
                    },
                })
                .collect();
            Reply {
                renewed: Some(renewed),
                ..Reply::ok()
            }
        }
        Request::CertsChanged => {
            state.certs.reload();
            Reply::ok()
        }
    }
}

async fn port_command(
    state: &Arc<State>,
    make: fn(Option<tokio::sync::oneshot::Sender<()>>) -> ports::Command,
) -> Reply {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if state.ports.send(make(Some(tx))).await.is_err() || rx.await.is_err() {
        return Reply::error("the server is shutting down");
    }
    Reply::ok()
}
