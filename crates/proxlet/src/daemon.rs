//! The server process: `proxlet run`.

use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proxlet_core::{ConfigFile, Locations, port};
use proxlet_server::{Engine, Snapshot};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};

use crate::control::{self, Reply, Request, StatusInfo, WireOutcome};
use crate::output::{CmdResult, Failure};
use crate::pipeline;

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
    let addr = SocketAddr::new(config.settings.listen_address, config.settings.http_port);
    let listener = bind_http(addr)?;
    let listen = listener.local_addr().unwrap_or(addr);
    let control_listener = bind_control(locations)?;

    let rules = snapshot.rule_count();
    let (stop, mut stop_rx) = watch::channel(false);
    let state = Arc::new(State {
        engine: Engine::new(snapshot),
        file,
        apply_lock: Mutex::new(()),
        started: Instant::now(),
        listen,
        run_by: std::env::var(SERVICE_ENV).unwrap_or_else(|_| "foreground".into()),
        stop,
    });

    tokio::spawn(control_loop(control_listener, state.clone()));
    tracing::info!(
        "proxlet {} listening on http://{listen} ({rules} rule{})",
        env!("CARGO_PKG_VERSION"),
        if rules == 1 { "" } else { "s" }
    );

    let shutdown = async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing a SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
            _ = stop_rx.changed() => {}
        }
        tracing::info!(
            "stopping; waiting up to {}s for in-flight requests",
            GRACE.as_secs()
        );
    };
    proxlet_server::serve(listener, state.engine.clone(), shutdown, GRACE).await;
    let _ = std::fs::remove_file(&socket);
    tracing::info!("stopped");
    Ok(())
}

fn bind_http(addr: SocketAddr) -> Result<tokio::net::TcpListener, Failure> {
    let port = addr.port();
    let listener = std::net::TcpListener::bind(addr).map_err(|e| match e.kind() {
        std::io::ErrorKind::AddrInUse => {
            let by = port::holder(port).map_or_else(String::new, |h| format!(" by {h}"));
            Failure::new(format!("port {port} is already in use{by}")).hint(
                "Stop whatever holds it, or move proxlet: proxlet config listen.http_port 8080",
            )
        }
        std::io::ErrorKind::PermissionDenied => {
            Failure::new(format!("binding port {port} needs elevated privileges"))
                .hint("Run proxlet as a system service: sudo proxlet start")
                .hint("Or use a high port: proxlet config listen.http_port 8080")
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
        Request::Status => Reply {
            status: Some(StatusInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                pid: std::process::id(),
                mode: "standalone".into(),
                listen: state.listen.to_string(),
                run_by: state.run_by.clone(),
                uptime_secs: state.started.elapsed().as_secs(),
                rules: state.engine.snapshot().rule_count(),
            }),
            ..Reply::ok()
        },
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
    }
}
