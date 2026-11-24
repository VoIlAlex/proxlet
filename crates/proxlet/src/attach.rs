//! Attaching behind a front server and detaching again: the parts of `start`,
//! `stop` and `run` that change the front server's config.
//!
//! What changed is recorded in a ledger in proxlet's state directory before
//! anything is written, so `stop` can undo it even after a crash.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use proxlet_core::Settings;
use proxlet_core::command::SystemRunner;
use proxlet_front::apply::{self, Entry};
use proxlet_front::{FrontServer, HttpsForward, Ledger, Location, Nginx};
use proxlet_service::{ServiceManager, UnitHook};

use crate::output::Failure;

/// The ledger's file name in the state directory.
pub const LEDGER: &str = "front.json";

/// Overrides the nginx binary proxlet uses (for tests and unusual installs).
pub const NGINX_ENV: &str = "PROXLET_NGINX";

/// The front server on this host.
#[must_use]
pub fn system_front() -> Box<dyn FrontServer> {
    match std::env::var(NGINX_ENV) {
        Ok(path) if !path.is_empty() => Box::new(Nginx::with_candidates(SystemRunner, &[&path])),
        _ => Box::new(Nginx::default()),
    }
}

/// The front server's name and config, for checks that only read it. `None`
/// when attach is off, no front server is installed, or its config cannot be
/// read (with a warning, since these checks are advisory).
#[must_use]
pub fn front_config(settings: &Settings) -> Option<(&'static str, proxlet_front::FrontConfig)> {
    if !settings.attach_enabled {
        return None;
    }
    let front = system_front();
    let inst = front.detect()?;
    match front.read(&inst) {
        Ok(cfg) => Some((front.name(), cfg)),
        Err(e) => {
            crate::output::warn(format!(
                "could not read {}'s config to check for conflicts: {e}",
                front.name()
            ));
            None
        }
    }
}

/// The internal address a front server forwards to.
#[must_use]
pub fn internal_addr(settings: &Settings) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], settings.attach_port))
}

/// What attaching did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `attach.enabled` is off.
    Disabled,
    /// No front server is installed.
    NoFront,
    /// proxlet's blocks were written; `moved_default` names where
    /// default-server flags were taken from.
    Attached {
        /// The file proxlet wrote.
        wrote: Option<PathBuf>,
        /// Where default-server flags were, if they were moved.
        moved_default: Vec<Location>,
        /// Default servers the user kept; their ports got no block from proxlet.
        kept_default: Vec<Location>,
    },
    /// Nothing to do: the same attach is already in place.
    AlreadyAttached,
    /// The user kept the front server's own default servers on every port,
    /// so proxlet wrote nothing.
    Declined {
        /// Where the first of them is.
        default_at: Location,
    },
}

/// Where the front server reads proxlet's certificates (design decision 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertDir {
    /// A directory root creates for the front server's workers: owned by the
    /// service user, group the workers' group, mode `2750`.
    Create(PathBuf),
    /// proxlet's own store, when the front server runs as the same user.
    Existing(PathBuf),
}

impl CertDir {
    fn path(&self) -> &Path {
        match self {
            Self::Create(p) | Self::Existing(p) => p,
        }
    }
}

/// Where `sudo proxlet start` publishes certificates for nginx's workers.
pub const PUBLISH_DIR: &str = "/var/lib/proxlet/nginx-certs";

/// The result of an attach, with what `start` needs to set up the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// The front server's name.
    pub front: &'static str,
    /// What happened.
    pub outcome: Outcome,
    /// The front server's error log, which the service may need to read.
    pub error_log: Option<PathBuf>,
    /// Why HTTPS is not forwarded, when the front server cannot do it.
    pub https_skipped: Option<String>,
}

/// Everything attach and detach need from the host.
pub struct Attach<'a> {
    /// The front server.
    pub front: &'a dyn FrontServer,
    /// The service manager that can hook the front server's unit, if any.
    pub manager: Option<&'a dyn ServiceManager>,
    /// Where the ledger lives.
    pub ledger: PathBuf,
    /// Who should own the ledger and its directory (the service user), when run as root.
    pub owner: Option<(u32, u32)>,
    /// The hook to install on the front server's unit.
    pub hook: Option<UnitHook>,
    /// Where the front server reads proxlet's certificates.
    pub cert_dir: CertDir,
    /// `--yes`: take over the default server without asking.
    pub yes: bool,
    /// Asks a yes/no question; `None` when there is no terminal.
    pub confirm: &'a dyn Fn(&str) -> Option<bool>,
}

fn rejected(front: &str, e: &proxlet_front::FrontError) -> Failure {
    Failure::new(format!(
        "{front} rejected proxlet's change; nothing was changed"
    ))
    .hint(e.to_string())
}

impl Attach<'_> {
    fn save(&self, ledger: &Ledger) -> Result<(), Failure> {
        if let Some(dir) = self.ledger.parent()
            && !dir.exists()
        {
            std::fs::create_dir_all(dir)?;
            self.chown(dir);
        }
        ledger.save(&self.ledger)?;
        self.chown(&self.ledger);
        Ok(())
    }

    fn chown(&self, path: &Path) {
        if let Some((uid, gid)) = self.owner {
            let _ = std::os::unix::fs::chown(path, Some(uid), Some(gid));
        }
    }

    fn ensure_hook(&self, ledger: &mut Ledger) -> Result<(), Failure> {
        if let (Some(manager), Some(unit), Some(hook)) =
            (self.manager, self.front.service_unit(), &self.hook)
        {
            manager.hook(unit, hook)?;
            ledger.hooked_unit = Some(unit.to_owned());
        }
        Ok(())
    }

    /// Attaches behind the front server, if one is installed.
    ///
    /// # Errors
    ///
    /// When the front server's config cannot be read, has no place for
    /// proxlet's block, or rejects the change (which is then undone).
    pub fn attach(&self, settings: &Settings) -> Result<Attached, Failure> {
        let front = self.front.name();
        let mut https_skipped = None;
        let done = |outcome, error_log, https_skipped| Attached {
            front,
            outcome,
            error_log,
            https_skipped,
        };
        if !settings.attach_enabled {
            return Ok(done(Outcome::Disabled, None, None));
        }
        let Some(inst) = self.front.detect() else {
            return Ok(done(Outcome::NoFront, None, None));
        };
        let port = settings.http_port;
        let internal = internal_addr(settings);
        let https = match self.front.https_support(&inst) {
            Ok(()) => Some(HttpsForward {
                port: settings.https_port,
                cert_dir: self.cert_dir.path().to_owned(),
            }),
            Err(why) => {
                https_skipped = Some(why);
                None
            }
        };
        let mut cfg = self.front.read(&inst)?;
        let error_log = self.front.error_log(&inst, &cfg);

        if self.keep_or_undo(&inst, &cfg, port, internal, https.as_ref())? {
            return Ok(done(Outcome::AlreadyAttached, error_log, https_skipped));
        }
        cfg = self.front.read(&inst)?;

        let ports: Vec<u16> = std::iter::once(port)
            .chain(https.as_ref().map(|h| h.port))
            .collect();
        let existing_defaults: Vec<Location> = ports
            .iter()
            .filter_map(|p| cfg.default_server(*p).map(|l| l.at.clone()))
            .collect();
        let take_default = existing_defaults.is_empty()
            || self.yes
            || (self.confirm)(&takeover_question(front, &existing_defaults)).unwrap_or(false);
        let mut ledger = Ledger {
            front: front.to_owned(),
            port,
            internal: internal.to_string(),
            ..Ledger::default()
        };
        let plan = self
            .front
            .plan_attach(&cfg, port, https.as_ref(), internal, take_default)?;
        if plan.is_empty()
            && let Some(default_at) = existing_defaults.first().cloned()
        {
            self.ensure_hook(&mut ledger)?;
            self.save(&ledger)?;
            return Ok(done(
                Outcome::Declined { default_at },
                error_log,
                https_skipped,
            ));
        }
        let (moved_default, kept_default) = if take_default {
            (existing_defaults, Vec::new())
        } else {
            (Vec::new(), existing_defaults)
        };
        // Planned the same way as `plan_attach`: kept defaults get no block.
        let forwarded_https =
            https.filter(|h| take_default || cfg.default_server(h.port).is_none());
        ledger.https_port = forwarded_https.as_ref().map(|h| h.port);
        ledger.cert_dir = forwarded_https.as_ref().map(|h| h.cert_dir.clone());
        ledger.entries = apply::prepare(&plan.steps)?;
        self.save(&ledger)?;
        if let Err(e) = apply::apply(self.front, &inst, &ledger.entries, self.front.is_running()) {
            let _ = std::fs::remove_file(&self.ledger);
            return Err(rejected(front, &e));
        }
        if forwarded_https.is_some()
            && let CertDir::Create(dir) = &self.cert_dir
            && !dir.exists()
        {
            self.create_cert_dir(dir, &cfg)?;
            ledger.cert_dir_created = true;
        }
        self.ensure_hook(&mut ledger)?;
        self.save(&ledger)?;
        let wrote = ledger.entries.iter().find_map(|e| match e {
            Entry::Wrote { path, .. } => Some(path.clone()),
            Entry::Edited { .. } => None,
        });
        Ok(done(
            Outcome::Attached {
                wrote,
                moved_default,
                kept_default,
            },
            error_log,
            https_skipped,
        ))
    }

    /// With the same attach already in place, refreshes its hook and returns
    /// true. With a different one (ports, internal address or HTTPS
    /// forwarding changed), undoes it so the caller starts over.
    fn keep_or_undo(
        &self,
        inst: &proxlet_front::Installation,
        cfg: &proxlet_front::FrontConfig,
        port: u16,
        internal: SocketAddr,
        https: Option<&HttpsForward>,
    ) -> Result<bool, Failure> {
        let Some(mut existing) = Ledger::load(&self.ledger)? else {
            return Ok(false);
        };
        let same = existing.port == port
            && existing.internal == internal.to_string()
            && existing.https_port == https.map(|h| h.port)
            && existing.cert_dir == https.map(|h| h.cert_dir.clone());
        if same && (cfg.has_managed_block() || existing.entries.is_empty()) {
            self.ensure_hook(&mut existing)?;
            self.save(&existing)?;
            return Ok(true);
        }
        apply::undo(self.front, inst, &existing.entries, self.front.is_running())?;
        std::fs::remove_file(&self.ledger)?;
        Ok(false)
    }

    /// Creates the directory the front server's workers read certificates
    /// from: the service user owns it, the workers' group can read it, and
    /// files created in it inherit that group.
    fn create_cert_dir(&self, dir: &Path, cfg: &proxlet_front::FrontConfig) -> Result<(), Failure> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir)?;
        let group = worker_gid(cfg).or(self.owner.map(|(_, gid)| gid));
        if let Some((uid, _)) = self.owner {
            let _ = std::os::unix::fs::chown(dir, Some(uid), group);
        } else if group.is_some() {
            let _ = std::os::unix::fs::chown(dir, None, group);
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o2750))?;
        Ok(())
    }

    /// Undoes everything the ledger records. Returns warnings about files the
    /// user changed since, or `None` when nothing was attached.
    ///
    /// # Errors
    ///
    /// When a file cannot be restored or the front server rejects the restored
    /// config; the ledger is kept so the user can fix it and try again.
    pub fn detach(&self) -> Result<Option<Vec<String>>, Failure> {
        let Some(ledger) = Ledger::load(&self.ledger)? else {
            return Ok(None);
        };
        if let (Some(manager), Some(unit)) = (self.manager, ledger.hooked_unit.as_deref()) {
            manager.unhook(unit)?;
        }
        let warnings = match self.front.detect() {
            Some(inst) => apply::undo(self.front, &inst, &ledger.entries, self.front.is_running())
                .map_err(|e| {
                    Failure::new(format!(
                        "could not restore {}'s config: {e}",
                        self.front.name()
                    ))
                    .hint("Fix the problem, then run: sudo proxlet stop")
                })?,
            None if ledger.entries.is_empty() => Vec::new(),
            None => vec![format!(
                "{} is no longer installed; its config files were left as they are",
                self.front.name()
            )],
        };
        if ledger.cert_dir_created
            && let Some(dir) = &ledger.cert_dir
        {
            let _ = std::fs::remove_dir_all(dir);
        }
        std::fs::remove_file(&self.ledger)?;
        Ok(Some(warnings))
    }
}

/// The question before taking over the front server's default servers.
fn takeover_question(front: &str, defaults: &[Location]) -> String {
    let at: Vec<String> = defaults.iter().map(ToString::to_string).collect();
    let (them, servers) = if at.len() == 1 {
        ("its default server at", "the default")
    } else {
        ("its default servers at", "the default")
    };
    format!(
        "{front} sends requests for hosts it does not serve to {them} {}. \
         Make proxlet {servers} instead?",
        at.join(" and ")
    )
}

/// The group the front server's workers run as: the `user` directive's
/// group, or its user's primary group (nginx's default user is `nobody`).
fn worker_gid(cfg: &proxlet_front::FrontConfig) -> Option<u32> {
    use nix::unistd::{Group, User};
    if let Some(g) = &cfg.worker_group {
        return Group::from_name(g).ok().flatten().map(|g| g.gid.as_raw());
    }
    let user = cfg.worker_user.as_deref().unwrap_or("nobody");
    User::from_name(user).ok().flatten().map(|u| u.gid.as_raw())
}

/// Whether the front server forwards HTTPS to proxlet, per the ledger.
#[must_use]
pub fn forwards_https(ledger: &Path) -> bool {
    Ledger::load(ledger)
        .ok()
        .flatten()
        .is_some_and(|l| l.https_port.is_some())
}

/// The directory the front server reads proxlet's certificates from, if
/// proxlet publishes them somewhere other than its own store.
#[must_use]
pub fn published_cert_dir(ledger: &Path, store_dir: &Path) -> Option<PathBuf> {
    let dir = Ledger::load(ledger).ok().flatten()?.cert_dir?;
    (dir != store_dir).then_some(dir)
}

/// The group that can read `log` when other users cannot, so the service can
/// follow it. `None` when anyone can read it or it does not exist.
#[must_use]
pub fn log_group(log: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(log).ok()?;
    if meta.mode() & 0o004 != 0 || meta.mode() & 0o040 == 0 {
        return None;
    }
    nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(meta.gid()))
        .ok()
        .flatten()
        .map(|g| g.name)
}
