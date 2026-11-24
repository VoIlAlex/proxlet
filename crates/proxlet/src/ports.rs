//! Who owns a public port (HTTP or HTTPS): proxlet, a front server such as
//! nginx, or something else. See design decision 1 of the `add-nginx-attach`
//! change; `add-cert` runs one owner per port (its decision 6).
//!
//! The public listener is the only thing that comes and goes. Rules, the
//! engine, the internal listener and every open connection are unaffected.

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use proxlet_core::port;
use proxlet_server::{Connections, Engine, Origin};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// How often a port proxlet does not hold is tried again.
pub const RETRY: Duration = Duration::from_secs(2);

/// After releasing the port for a front server, how long proxlet waits for it
/// to bind before taking the port back regardless.
pub const HOLD: Duration = Duration::from_secs(10);

/// The waiting reason shown before the first check of who holds the port.
pub const CHECKING: &str = "checking who holds it";

/// The state of the public port, as seen by `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// proxlet listens on the port itself.
    Standalone,
    /// A front server holds the port and forwards to the internal listener.
    Attached {
        /// The front server's name, e.g. `nginx`.
        front: &'static str,
    },
    /// Something else holds the port, or binding fails; proxlet retries.
    Waiting {
        /// Why, e.g. `held by caddy`.
        reason: String,
    },
    /// proxlet let go of the port so a front server can take it.
    Released,
}

impl Mode {
    /// The `mode` line of `proxlet status`.
    #[must_use]
    pub fn label(&self, port: u16) -> String {
        match self {
            Self::Standalone => "standalone".into(),
            Self::Attached { front } => format!("attached ({front})"),
            Self::Waiting { reason } => format!("waiting for port {port} ({reason})"),
            Self::Released => format!("handing port {port} over"),
        }
    }
}

/// Who holds the port after a failed bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A supported front server.
    Front(&'static str),
    /// Any other process, if it could be identified.
    Other(Option<port::Holder>),
}

/// Facts about the host the owner needs; injectable for tests.
pub trait Probe: Send + Sync + 'static {
    /// Who holds `port`. With `attach` off, a front server counts as any other process.
    fn classify(&self, port: u16, attach: bool) -> Holder;
    /// Whether a front server process is running right now.
    fn front_running(&self) -> bool;
}

/// Opens the public listener.
pub type Binder = Box<dyn Fn() -> io::Result<std::net::TcpListener> + Send + Sync>;

/// A request to the owner.
#[derive(Debug)]
pub enum Command {
    /// Release the port for a front server. Replies once the listener is closed.
    Release(Option<oneshot::Sender<()>>),
    /// Try to take the port now. Replies after the attempt.
    Reclaim(Option<oneshot::Sender<()>>),
}

/// The task that owns the public listener.
pub struct Owner {
    /// Opens the public listener.
    pub binder: Binder,
    /// Host facts.
    pub probe: Arc<dyn Probe>,
    /// The engine both listeners serve.
    pub engine: Arc<Engine>,
    /// Shared connection set, drained on shutdown.
    pub conns: Connections,
    /// The public port, for messages.
    pub port: u16,
    /// Retry interval while the port is not ours.
    pub retry: Duration,
    /// See [`HOLD`].
    pub hold: Duration,
    /// For the HTTPS port: the TLS configuration connections are served with.
    pub tls: Option<Arc<proxlet_server::tls::ServerConfig>>,
}

struct Accepting {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Accepting {
    async fn close(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

/// Builds a command from its reply channel.
type MakeCommand = fn(Option<oneshot::Sender<()>>) -> Command;

/// Forwards each command to every owner, replying once all of them have, so
/// a release frees the HTTP and the HTTPS port together. Ends when `commands`
/// closes.
pub async fn fan_out(mut commands: mpsc::Receiver<Command>, owners: Vec<mpsc::Sender<Command>>) {
    while let Some(cmd) = commands.recv().await {
        let (to, make): (_, MakeCommand) = match cmd {
            Command::Release(to) => (to, Command::Release),
            Command::Reclaim(to) => (to, Command::Reclaim),
        };
        let mut waits = Vec::new();
        for owner in &owners {
            let (tx, rx) = oneshot::channel();
            if owner.send(make(Some(tx))).await.is_ok() {
                waits.push(rx);
            }
        }
        for rx in waits {
            let _ = rx.await;
        }
        reply(to);
    }
}

fn reply(to: Option<oneshot::Sender<()>>) {
    if let Some(r) = to {
        let _ = r.send(());
    }
}

impl Owner {
    /// Runs until `shutdown` resolves, publishing every mode change on `mode`.
    ///
    /// `first` is the result of the startup bind, so the caller can still
    /// fail fast on errors other than a taken port.
    pub async fn run(
        self,
        first: Option<std::net::TcpListener>,
        mode: watch::Sender<Mode>,
        mut commands: mpsc::Receiver<Command>,
        shutdown: impl Future<Output = ()>,
    ) {
        tokio::pin!(shutdown);
        let mut accepting = first.map(|l| self.accept(l));
        let mut released_at: Option<Instant> = None;
        let mut last_logged: Option<Mode> = None;
        let mut attempt_now = accepting.is_none();
        if accepting.is_some() {
            mode.send_replace(Mode::Standalone);
        }

        loop {
            if let Some(current) = accepting.take() {
                tokio::select! {
                    () = &mut shutdown => {
                        current.close().await;
                        return;
                    }
                    cmd = commands.recv() => match cmd {
                        Some(Command::Release(to)) => {
                            current.close().await;
                            tracing::info!("released port {} for a front server", self.port);
                            released_at = Some(Instant::now());
                            mode.send_replace(Mode::Released);
                            reply(to);
                        }
                        Some(Command::Reclaim(to)) => {
                            accepting = Some(current);
                            reply(to);
                        }
                        None => accepting = Some(current),
                    },
                }
                continue;
            }

            let mut pending = None;
            if !std::mem::take(&mut attempt_now) {
                tokio::select! {
                    () = &mut shutdown => return,
                    () = tokio::time::sleep(self.retry) => {}
                    cmd = commands.recv() => match cmd {
                        Some(Command::Release(to)) => {
                            // Not ours already; keep a retry from grabbing it back early.
                            released_at = Some(Instant::now());
                            reply(to);
                            continue;
                        }
                        Some(Command::Reclaim(to)) => {
                            released_at = None;
                            pending = to;
                        }
                        None => {}
                    },
                }
            }

            if let Some(at) = released_at
                && at.elapsed() < self.hold
                && self.front_running().await
            {
                reply(pending);
                continue;
            }

            match (self.binder)() {
                Ok(listener) => {
                    released_at = None;
                    last_logged = None;
                    accepting = Some(self.accept(listener));
                    tracing::info!("listening on port {}", self.port);
                    mode.send_replace(Mode::Standalone);
                }
                Err(e) => {
                    let (next, pid) = self.explain(&e).await;
                    if last_logged.as_ref() != Some(&next) {
                        self.log(&next, pid);
                        last_logged = Some(next.clone());
                    }
                    mode.send_replace(next);
                }
            }
            reply(pending);
        }
    }

    fn accept(&self, listener: std::net::TcpListener) -> Accepting {
        let (stop, stopped) = oneshot::channel::<()>();
        let listener = listener
            .set_nonblocking(true)
            .and_then(|()| tokio::net::TcpListener::from_std(listener));
        let engine = self.engine.clone();
        let conns = self.conns.clone();
        let tls = self.tls.clone();
        let task = tokio::spawn(async move {
            let stop = async {
                let _ = stopped.await;
            };
            match (listener, tls) {
                (Ok(l), None) => {
                    proxlet_server::accept(l, engine, conns, Origin::Direct, stop).await;
                }
                (Ok(l), Some(tls)) => proxlet_server::accept_tls(l, engine, conns, tls, stop).await,
                (Err(e), _) => tracing::error!("cannot use the public listener: {e}"),
            }
        });
        Accepting { stop, task }
    }

    async fn front_running(&self) -> bool {
        let probe = self.probe.clone();
        tokio::task::spawn_blocking(move || probe.front_running())
            .await
            .unwrap_or(false)
    }

    async fn explain(&self, err: &io::Error) -> (Mode, Option<u32>) {
        if err.kind() != io::ErrorKind::AddrInUse {
            let reason = match err.kind() {
                io::ErrorKind::PermissionDenied => "binding needs elevated privileges".into(),
                _ => err.to_string(),
            };
            return (Mode::Waiting { reason }, None);
        }
        let probe = self.probe.clone();
        let port = self.port;
        let attach = self.engine.snapshot().settings().attach_enabled;
        let holder = tokio::task::spawn_blocking(move || probe.classify(port, attach))
            .await
            .unwrap_or(Holder::Other(None));
        match holder {
            Holder::Front(front) => (Mode::Attached { front }, None),
            Holder::Other(h) => (
                Mode::Waiting {
                    reason: format!(
                        "held by {}",
                        h.as_ref().map_or("another process", |h| h.name.as_str())
                    ),
                },
                h.map(|h| h.pid),
            ),
        }
    }

    fn log(&self, mode: &Mode, pid: Option<u32>) {
        let port = self.port;
        match mode {
            Mode::Attached { front } => {
                tracing::info!("port {port} is held by {front}; serving behind it");
            }
            Mode::Waiting { reason } => {
                if let Some(by) = reason.strip_prefix("held by ") {
                    tracing::error!(
                        "port {port} is already in use by {by}{}; waiting for it to be free",
                        pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
                    );
                } else {
                    tracing::error!("cannot listen on port {port}: {reason}; retrying");
                }
            }
            Mode::Standalone | Mode::Released => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use proxlet_core::Config;
    use proxlet_server::Snapshot;

    use super::*;

    // Short real intervals: the probe runs on blocking threads, which a paused clock skips past.
    const R: Duration = Duration::from_millis(40);
    const H: Duration = Duration::from_millis(400);

    /// A port slot that can be "taken" by someone else, backed by real sockets.
    struct Fake {
        taken_by: Mutex<Option<Holder>>,
        front: AtomicBool,
        addr: Mutex<Option<std::net::SocketAddr>>,
    }

    impl Probe for Fake {
        fn classify(&self, _port: u16, attach: bool) -> Holder {
            match self.taken_by.lock().unwrap().clone() {
                Some(Holder::Front(f)) if attach => Holder::Front(f),
                Some(Holder::Front(f)) => Holder::Other(Some(port::Holder {
                    pid: 1,
                    name: f.into(),
                })),
                Some(h) => h,
                None => Holder::Other(None),
            }
        }
        fn front_running(&self) -> bool {
            self.front.load(Ordering::SeqCst)
        }
    }

    struct Rig {
        fake: Arc<Fake>,
        mode: watch::Receiver<Mode>,
        commands: mpsc::Sender<Command>,
        stop: Option<oneshot::Sender<()>>,
        task: JoinHandle<()>,
    }

    fn rig(start_taken: Option<&Holder>, attach: bool) -> Rig {
        let fake = Arc::new(Fake {
            taken_by: Mutex::new(start_taken.cloned()),
            front: AtomicBool::new(false),
            addr: Mutex::new(None),
        });
        let mut config = Config::default();
        config.settings.attach_enabled = attach;
        let engine = Engine::new(Snapshot::build(&config).unwrap());
        let binder_fake = fake.clone();
        let binder: Binder = Box::new(move || {
            if binder_fake.taken_by.lock().unwrap().is_some() {
                return Err(io::ErrorKind::AddrInUse.into());
            }
            let l = std::net::TcpListener::bind("127.0.0.1:0")?;
            *binder_fake.addr.lock().unwrap() = Some(l.local_addr()?);
            Ok(l)
        });
        let first = if start_taken.is_none() {
            Some(binder().unwrap())
        } else {
            None
        };
        let owner = Owner {
            binder,
            probe: fake.clone(),
            engine,
            conns: Connections::new(),
            port: 80,
            retry: R,
            hold: H,
            tls: None,
        };
        let (mode_tx, mode) = watch::channel(Mode::Released);
        let (commands, rx) = mpsc::channel(8);
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(owner.run(first, mode_tx, rx, async {
            let _ = stopped.await;
        }));
        Rig {
            fake,
            mode,
            commands,
            stop: Some(stop),
            task,
        }
    }

    impl Rig {
        async fn settle(&self) {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        fn mode(&self) -> Mode {
            self.mode.borrow().clone()
        }
        async fn release(&self) {
            let (tx, rx) = oneshot::channel();
            self.commands
                .send(Command::Release(Some(tx)))
                .await
                .unwrap();
            rx.await.unwrap();
        }
        async fn finish(mut self) {
            let _ = self.stop.take().unwrap().send(());
            self.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn free_port_is_standalone() {
        let r = rig(None, true);
        r.settle().await;
        assert_eq!(r.mode(), Mode::Standalone);
        r.finish().await;
    }

    #[tokio::test]
    async fn port_held_by_front_server_means_attached() {
        let r = rig(Some(&Holder::Front("nginx")), true);
        r.settle().await;
        assert_eq!(r.mode(), Mode::Attached { front: "nginx" });
        assert_eq!(r.mode().label(80), "attached (nginx)");
        r.finish().await;
    }

    #[tokio::test]
    async fn port_held_by_another_process_waits_then_takes_it() {
        let other = Holder::Other(Some(port::Holder {
            pid: 42,
            name: "caddy".into(),
        }));
        let r = rig(Some(&other), true);
        r.settle().await;
        assert_eq!(r.mode().label(80), "waiting for port 80 (held by caddy)");
        *r.fake.taken_by.lock().unwrap() = None;
        tokio::time::sleep(R * 2).await;
        r.settle().await;
        assert_eq!(r.mode(), Mode::Standalone);
        r.finish().await;
    }

    #[tokio::test]
    async fn attach_off_treats_nginx_like_anything_else() {
        let r = rig(Some(&Holder::Front("nginx")), false);
        r.settle().await;
        assert_eq!(r.mode().label(80), "waiting for port 80 (held by nginx)");
        r.finish().await;
    }

    #[tokio::test]
    async fn release_closes_the_listener_and_front_takes_over() {
        let r = rig(None, true);
        r.settle().await;
        let addr = r.fake.addr.lock().unwrap().unwrap();
        r.fake.front.store(true, Ordering::SeqCst);
        r.release().await;
        assert_eq!(r.mode(), Mode::Released);
        assert!(
            std::net::TcpListener::bind(addr).is_ok(),
            "listener still open"
        );

        // nginx binds; the next retry sees it.
        *r.fake.taken_by.lock().unwrap() = Some(Holder::Front("nginx"));
        tokio::time::sleep(H + R * 2).await;
        r.settle().await;
        assert_eq!(r.mode(), Mode::Attached { front: "nginx" });

        // nginx stops: proxlet reclaims within one retry.
        *r.fake.taken_by.lock().unwrap() = None;
        r.fake.front.store(false, Ordering::SeqCst);
        tokio::time::sleep(R * 2).await;
        r.settle().await;
        assert_eq!(r.mode(), Mode::Standalone);
        r.finish().await;
    }

    #[tokio::test]
    async fn release_is_held_while_the_front_server_starts() {
        let r = rig(None, true);
        r.settle().await;
        r.fake.front.store(true, Ordering::SeqCst);
        r.release().await;
        // The port is free, but nginx is still starting: proxlet keeps off it.
        tokio::time::sleep(R * 3).await;
        r.settle().await;
        assert_eq!(r.mode(), Mode::Released);
        // nginx never binds: after the hold, proxlet takes the port back.
        tokio::time::sleep(H).await;
        r.settle().await;
        assert_eq!(r.mode(), Mode::Standalone);
        r.finish().await;
    }

    #[tokio::test]
    async fn failed_front_start_is_reclaimed_quickly() {
        let r = rig(None, true);
        r.settle().await;
        r.fake.front.store(true, Ordering::SeqCst);
        r.release().await;
        // e.g. `nginx -t`: the process goes away without binding.
        r.fake.front.store(false, Ordering::SeqCst);
        tokio::time::sleep(R * 2).await;
        r.settle().await;
        assert_eq!(r.mode(), Mode::Standalone);
        r.finish().await;
    }

    #[tokio::test]
    async fn reclaim_tries_at_once() {
        let r = rig(Some(&Holder::Front("nginx")), true);
        r.settle().await;
        *r.fake.taken_by.lock().unwrap() = None;
        let (tx, rx) = oneshot::channel();
        r.commands.send(Command::Reclaim(Some(tx))).await.unwrap();
        rx.await.unwrap();
        assert_eq!(r.mode(), Mode::Standalone);
        r.finish().await;
    }
}
