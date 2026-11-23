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
use proxlet_front::{FrontServer, Ledger, Location, Nginx};
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
    /// proxlet's block was written; `moved_default` names where the
    /// default-server flag was taken from.
    Attached {
        /// The file proxlet wrote.
        wrote: Option<PathBuf>,
        /// Where the default-server flag was, if it was moved.
        moved_default: Vec<Location>,
    },
    /// Nothing to do: the same attach is already in place.
    AlreadyAttached,
    /// The user kept the front server's own default server.
    Declined {
        /// Where it is.
        default_at: Location,
    },
}

/// The result of an attach, with what `start` needs to set up the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// The front server's name.
    pub front: &'static str,
    /// What happened.
    pub outcome: Outcome,
    /// The front server's error log, which the service may need to read.
    pub error_log: Option<PathBuf>,
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
        let done = |outcome, error_log| Attached {
            front,
            outcome,
            error_log,
        };
        if !settings.attach_enabled {
            return Ok(done(Outcome::Disabled, None));
        }
        let Some(inst) = self.front.detect() else {
            return Ok(done(Outcome::NoFront, None));
        };
        let port = settings.http_port;
        let internal = internal_addr(settings);
        let mut cfg = self.front.read(&inst)?;
        let error_log = self.front.error_log(&inst, &cfg);

        if let Some(mut existing) = Ledger::load(&self.ledger)? {
            let same = existing.port == port && existing.internal == internal.to_string();
            if same && (cfg.has_managed_block() || existing.entries.is_empty()) {
                self.ensure_hook(&mut existing)?;
                self.save(&existing)?;
                return Ok(done(Outcome::AlreadyAttached, error_log));
            }
            // The port or internal address changed: start over.
            apply::undo(
                self.front,
                &inst,
                &existing.entries,
                self.front.is_running(),
            )?;
            std::fs::remove_file(&self.ledger)?;
            cfg = self.front.read(&inst)?;
        }

        let existing_default = cfg.default_server(port).map(|l| l.at.clone());
        let take_default = match &existing_default {
            None => true,
            Some(_) if self.yes => true,
            Some(at) => (self.confirm)(&format!(
                "{front} sends requests for hosts it does not serve to its default server at {at}. \
                 Make proxlet the default instead?"
            ))
            .unwrap_or(false),
        };
        let mut ledger = Ledger {
            front: front.to_owned(),
            port,
            internal: internal.to_string(),
            hooked_unit: None,
            entries: Vec::new(),
        };
        if let (false, Some(default_at)) = (take_default, existing_default) {
            self.ensure_hook(&mut ledger)?;
            self.save(&ledger)?;
            return Ok(done(Outcome::Declined { default_at }, error_log));
        }

        let moved_default = cfg
            .servers
            .iter()
            .filter(|s| !s.managed)
            .flat_map(|s| &s.listens)
            .filter(|l| l.port == port && l.default_flag.is_some())
            .map(|l| l.at.clone())
            .collect();
        let plan = self.front.plan_attach(&cfg, port, internal, true)?;
        ledger.entries = apply::prepare(&plan.steps)?;
        self.save(&ledger)?;
        if let Err(e) = apply::apply(self.front, &inst, &ledger.entries, self.front.is_running()) {
            let _ = std::fs::remove_file(&self.ledger);
            return Err(rejected(front, &e));
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
            },
            error_log,
        ))
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
        std::fs::remove_file(&self.ledger)?;
        Ok(Some(warnings))
    }
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
