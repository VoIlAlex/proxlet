//! Where proxlet keeps its configuration file, state and control socket.
//!
//! Resolution order:
//!
//! 1. `PROXLET_HOME` puts everything under one directory (tests, CI, sandboxes).
//! 2. `PROXLET_CONFIG_DIR` / `PROXLET_STATE_DIR` override one location each; the
//!    system service sets both so the daemon uses exactly the directories the
//!    CLI resolved when it was installed.
//! 3. Running as root through `sudo` (with `SUDO_USER` set) uses the invoking
//!    user's directories, so `sudo proxlet ...` manages the same rules.
//! 4. Running as root otherwise uses system directories (`/etc/proxlet`,
//!    `/var/lib/proxlet`).
//! 5. Everyone else uses `$XDG_CONFIG_HOME/proxlet` (default `~/.config/proxlet`)
//!    and `$XDG_STATE_HOME/proxlet` (default `~/.local/state/proxlet`).
//!
//! The control socket lives in the state directory rather than
//! `$XDG_RUNTIME_DIR`: a system service does not get the user's runtime
//! directory, and the CLI and the daemon must agree on the socket path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Resolved file locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locations {
    /// Directory holding `proxlet.toml`.
    pub config_dir: PathBuf,
    /// Directory for state (socket, and later logs and certificates).
    pub state_dir: PathBuf,
}

impl Locations {
    /// The configuration file.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("proxlet.toml")
    }

    /// The control socket.
    ///
    /// Unix socket paths are limited to about 100 bytes (104 on macOS). When
    /// the state directory is too deep, the socket moves to a short per-user
    /// directory under `/tmp`, named by a hash of the full path so the CLI and
    /// the daemon still agree on it.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        let path = self.state_dir.join("proxlet.sock");
        if path.as_os_str().len() <= MAX_SOCKET_PATH {
            return path;
        }
        let hash = fnv1a(path.as_os_str().as_encoded_bytes());
        short_socket_dir().join(format!("{hash:016x}.sock"))
    }

    /// Whether [`Self::socket`] had to move the socket to the short `/tmp` directory.
    /// The daemon creates that directory with mode 0700 and refuses it if someone else owns it.
    #[must_use]
    pub fn socket_is_shortened(&self) -> bool {
        self.socket().parent() == Some(short_socket_dir().as_path())
    }

    /// Everything under one directory.
    #[must_use]
    pub fn under(home: &Path) -> Self {
        Self {
            config_dir: home.to_owned(),
            state_dir: home.join("state"),
        }
    }

    /// The default per-user locations for a home directory, ignoring the
    /// environment. Used when installing the service for another user.
    #[must_use]
    pub fn for_user_home(home: &Path) -> Self {
        Self {
            config_dir: home.join(".config").join("proxlet"),
            state_dir: home.join(".local").join("state").join("proxlet"),
        }
    }

    /// System-wide locations used when running as root without `sudo`.
    #[must_use]
    pub fn system() -> Self {
        Self {
            config_dir: PathBuf::from("/etc/proxlet"),
            state_dir: PathBuf::from("/var/lib/proxlet"),
        }
    }

    /// Resolves locations for the current process.
    #[must_use]
    pub fn resolve() -> Self {
        Self::resolve_with(
            |k| std::env::var_os(k),
            nix::unistd::geteuid().is_root(),
            sudo_user_home,
        )
    }

    /// Resolution with every input injected, for tests.
    pub fn resolve_with(
        env: impl Fn(&str) -> Option<OsString>,
        is_root: bool,
        home_of: impl Fn(&str) -> Option<PathBuf>,
    ) -> Self {
        let non_empty = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);

        if let Some(home) = non_empty("PROXLET_HOME") {
            return Self::under(&home);
        }

        let mut base = if is_root {
            match env("SUDO_USER")
                .and_then(|u| u.into_string().ok())
                .filter(|u| !u.is_empty() && u != "root")
                .and_then(|u| home_of(&u))
            {
                Some(home) => Self::for_user_home(&home),
                None => Self::system(),
            }
        } else {
            let home = non_empty("HOME").unwrap_or_else(|| PathBuf::from("/"));
            Self {
                config_dir: non_empty("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| home.join(".config"))
                    .join("proxlet"),
                state_dir: non_empty("XDG_STATE_HOME")
                    .unwrap_or_else(|| home.join(".local").join("state"))
                    .join("proxlet"),
            }
        };

        if let Some(dir) = non_empty("PROXLET_CONFIG_DIR") {
            base.config_dir = dir;
        }
        if let Some(dir) = non_empty("PROXLET_STATE_DIR") {
            base.state_dir = dir;
        }
        base
    }
}

/// Longest socket path used as-is; leaves headroom under every platform's limit.
const MAX_SOCKET_PATH: usize = 100;

/// `/tmp/proxlet-<uid>`, using the invoking user's uid under `sudo`.
fn short_socket_dir() -> PathBuf {
    let euid = nix::unistd::geteuid();
    let uid = if euid.is_root() {
        std::env::var("SUDO_UID")
            .ok()
            .and_then(|u| u.parse::<u32>().ok())
            .unwrap_or(0)
    } else {
        euid.as_raw()
    };
    PathBuf::from(format!("/tmp/proxlet-{uid}"))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn sudo_user_home(name: &str) -> Option<PathBuf> {
    nix::unistd::User::from_name(name)
        .ok()
        .flatten()
        .map(|u| u.dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(vars: &[(&str, &str)], is_root: bool) -> Locations {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        Locations::resolve_with(
            |k| vars.get(k).cloned(),
            is_root,
            |u| (u == "alice").then(|| PathBuf::from("/home/alice")),
        )
    }

    #[test]
    fn proxlet_home_wins() {
        let l = resolve(&[("PROXLET_HOME", "/tmp/p"), ("HOME", "/home/me")], true);
        assert_eq!(l.config_file(), PathBuf::from("/tmp/p/proxlet.toml"));
        assert_eq!(l.socket(), PathBuf::from("/tmp/p/state/proxlet.sock"));
    }

    #[test]
    fn per_user_defaults() {
        let l = resolve(&[("HOME", "/home/me")], false);
        assert_eq!(
            l.config_file(),
            PathBuf::from("/home/me/.config/proxlet/proxlet.toml")
        );
        assert_eq!(
            l.socket(),
            PathBuf::from("/home/me/.local/state/proxlet/proxlet.sock")
        );
    }

    #[test]
    fn per_user_xdg() {
        let l = resolve(
            &[
                ("HOME", "/home/me"),
                ("XDG_CONFIG_HOME", "/cfg"),
                ("XDG_STATE_HOME", "/st"),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ],
            false,
        );
        assert_eq!(l.config_dir, PathBuf::from("/cfg/proxlet"));
        assert_eq!(l.socket(), PathBuf::from("/st/proxlet/proxlet.sock"));
    }

    #[test]
    fn sudo_uses_invoking_user() {
        let l = resolve(&[("SUDO_USER", "alice"), ("HOME", "/root")], true);
        assert_eq!(l, Locations::for_user_home(Path::new("/home/alice")));
    }

    #[test]
    fn plain_root_uses_system_dirs() {
        assert_eq!(resolve(&[("HOME", "/root")], true), Locations::system());
        assert_eq!(resolve(&[("SUDO_USER", "root")], true), Locations::system());
        assert_eq!(
            resolve(&[("SUDO_USER", "ghost")], true),
            Locations::system()
        );
    }

    #[test]
    fn long_socket_paths_are_shortened_deterministically() {
        let deep = "/x".repeat(60);
        let l = Locations::under(Path::new(&deep));
        let socket = l.socket();
        assert!(socket.as_os_str().len() <= MAX_SOCKET_PATH, "{socket:?}");
        assert!(l.socket_is_shortened());
        assert_eq!(socket, Locations::under(Path::new(&deep)).socket());
        let other = Locations::under(Path::new(&format!("{deep}y")));
        assert_ne!(socket, other.socket());
        assert!(!Locations::under(Path::new("/tmp/p")).socket_is_shortened());
    }

    #[test]
    fn service_overrides() {
        let l = resolve(
            &[
                ("HOME", "/home/alice"),
                ("PROXLET_CONFIG_DIR", "/c"),
                ("PROXLET_STATE_DIR", "/s"),
            ],
            false,
        );
        assert_eq!(l.config_dir, PathBuf::from("/c"));
        assert_eq!(l.state_dir, PathBuf::from("/s"));
    }
}
