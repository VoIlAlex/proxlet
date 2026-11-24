//! Applying a [`Plan`](crate::Plan) to a front server's config, and undoing it.
//!
//! This part is the same for every front server. A change is first turned into
//! ledger [`Entry`]s, which record what each file held before; the caller
//! persists them before anything is written, so a crash part-way through can
//! still be undone. Files are written atomically, the front server's config is
//! checked, a running front server is reloaded, and every file is restored if
//! any step fails.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{FrontError, FrontServer, Installation};

/// One change to a front server's config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Create or replace a file.
    Write {
        /// The file.
        path: PathBuf,
        /// Its new contents.
        contents: String,
    },
    /// Replace one line of an existing file.
    EditLine {
        /// The file (with symlinks resolved).
        path: PathBuf,
        /// 1-based line number where `from` was read.
        line: usize,
        /// The line as it is.
        from: String,
        /// The line as it should be.
        to: String,
    },
}

/// One recorded change, enough to undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// proxlet wrote `contents` to `path`, which held `previous` (or did not exist).
    Wrote {
        /// The file.
        path: PathBuf,
        /// What it held before, if it existed.
        previous: Option<String>,
        /// What proxlet wrote.
        contents: String,
    },
    /// proxlet replaced line `line` of `path`, `from`, with `to`.
    Edited {
        /// The file.
        path: PathBuf,
        /// 1-based line.
        line: usize,
        /// The original line.
        from: String,
        /// proxlet's version of it.
        to: String,
    },
}

/// Everything proxlet changed to attach to a front server, saved in proxlet's
/// state directory so `stop` can undo it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// The front server, e.g. `nginx`.
    pub front: String,
    /// The public port proxlet's block listens on.
    pub port: u16,
    /// The internal address proxlet's block forwards to.
    pub internal: String,
    /// The service unit proxlet hooked, if any.
    #[serde(default)]
    pub hooked_unit: Option<String>,
    /// The HTTPS port proxlet's block listens on, when HTTPS is forwarded.
    /// Ledgers from before HTTPS forwarding have none.
    #[serde(default)]
    pub https_port: Option<u16>,
    /// Where the front server reads proxlet's certificates, when HTTPS is
    /// forwarded; `stop` removes it if proxlet created it.
    #[serde(default)]
    pub cert_dir: Option<std::path::PathBuf>,
    /// Whether proxlet created `cert_dir` (and so may remove it).
    #[serde(default)]
    pub cert_dir_created: bool,
    /// The config changes, in the order they were made.
    pub entries: Vec<Entry>,
}

impl Ledger {
    /// Reads a ledger; `None` when there is none.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Option<Self>, FrontError> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| FrontError::Io {
                    path: path.to_owned(),
                    source: io::Error::new(io::ErrorKind::InvalidData, e),
                }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(FrontError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Writes the ledger atomically.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), FrontError> {
        let text = serde_json::to_string_pretty(self).map_err(|e| FrontError::Io {
            path: path.to_owned(),
            source: io::Error::other(e),
        })?;
        write_atomic(path, &text)
    }
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> FrontError + '_ {
    move |source| FrontError::Io {
        path: path.to_owned(),
        source,
    }
}

fn read_opt(path: &Path) -> Result<Option<String>, FrontError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

/// Writes through a temporary file and a rename, keeping the file's mode.
fn write_atomic(path: &Path, contents: &str) -> Result<(), FrontError> {
    let mode = fs::metadata(path).map_or(0o644, |m| m.permissions().mode() & 0o7777);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.proxlet-tmp"));
    let result = (|| {
        fs::write(&tmp, contents)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io_err(path))
}

fn set_contents(path: &Path, contents: Option<&str>) -> Result<(), FrontError> {
    match contents {
        Some(c) => write_atomic(path, c),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(path)(e)),
        },
    }
}

/// The index of the line equal to `want`: at `line` if it is still there,
/// otherwise the only such line in the file.
fn find_line(lines: &[&str], line: usize, want: &str) -> Option<usize> {
    if lines.get(line.wrapping_sub(1)) == Some(&want) {
        return Some(line - 1);
    }
    let mut found = lines.iter().enumerate().filter(|(_, l)| **l == want);
    let first = found.next()?.0;
    found.next().is_none().then_some(first)
}

fn replace_line(text: &str, index: usize, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == index {
            out.push_str(with);
            if l.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(l);
        }
    }
    out
}

/// Turns plan steps into ledger entries, reading what each file holds now.
/// Writes that would change nothing are dropped.
///
/// # Errors
///
/// When a file cannot be read, or an edited line is no longer there.
pub fn prepare(steps: &[Step]) -> Result<Vec<Entry>, FrontError> {
    let mut entries = Vec::new();
    for step in steps {
        match step {
            Step::Write { path, contents } => {
                let previous = read_opt(path)?;
                if previous.as_deref() != Some(contents) {
                    entries.push(Entry::Wrote {
                        path: path.clone(),
                        previous,
                        contents: contents.clone(),
                    });
                }
            }
            Step::EditLine {
                path,
                line,
                from,
                to,
            } => {
                let text = fs::read_to_string(path).map_err(io_err(path))?;
                let lines: Vec<&str> = text.lines().collect();
                let index = find_line(&lines, *line, from).ok_or_else(|| {
                    FrontError::Layout(format!(
                        "{}:{line} changed while proxlet was reading it; run the command again",
                        path.display()
                    ))
                })?;
                entries.push(Entry::Edited {
                    path: path.clone(),
                    line: index + 1,
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
    }
    Ok(entries)
}

fn perform(entry: &Entry) -> Result<(), FrontError> {
    match entry {
        Entry::Wrote { path, contents, .. } => write_atomic(path, contents),
        Entry::Edited {
            path,
            line,
            from,
            to,
        } => {
            let text = fs::read_to_string(path).map_err(io_err(path))?;
            let lines: Vec<&str> = text.lines().collect();
            let index = find_line(&lines, *line, from).ok_or_else(|| {
                FrontError::Layout(format!(
                    "{}:{line} changed; nothing was edited",
                    path.display()
                ))
            })?;
            write_atomic(path, &replace_line(&text, index, to))
        }
    }
}

/// Puts back what an entry replaced, if the file still holds proxlet's version.
/// Returns a warning when the user changed it since.
fn revert(entry: &Entry) -> Result<Option<String>, FrontError> {
    match entry {
        Entry::Wrote {
            path,
            previous,
            contents,
        } => {
            let current = read_opt(path)?;
            if current.as_deref() == Some(contents.as_str()) {
                set_contents(path, previous.as_deref())?;
                Ok(None)
            } else if current.is_none() || current == *previous {
                Ok(None)
            } else {
                Ok(Some(format!(
                    "{} was changed after proxlet wrote it; left as it is",
                    path.display()
                )))
            }
        }
        Entry::Edited {
            path,
            line,
            from,
            to,
        } => {
            let Some(text) = read_opt(path)? else {
                return Ok(Some(format!(
                    "{} no longer exists; nothing to restore",
                    path.display()
                )));
            };
            let lines: Vec<&str> = text.lines().collect();
            if let Some(index) = find_line(&lines, *line, to) {
                write_atomic(path, &replace_line(&text, index, from))?;
                return Ok(None);
            }
            if find_line(&lines, *line, from).is_some() {
                return Ok(None);
            }
            Ok(Some(format!(
                "{}:{line} no longer reads `{}`; left as it is (it was `{}` before proxlet)",
                path.display(),
                to.trim(),
                from.trim()
            )))
        }
    }
}

fn revert_all(entries: &[Entry]) {
    for e in entries.iter().rev() {
        let _ = revert(e);
    }
}

/// Makes the changes, checks the config and reloads a running front server.
/// On any failure every file is put back and the error is returned.
///
/// # Errors
///
/// The first failure: a write, the check (with the front server's message) or the reload.
pub fn apply(
    front: &dyn FrontServer,
    inst: &Installation,
    entries: &[Entry],
    reload: bool,
) -> Result<(), FrontError> {
    for (done, entry) in entries.iter().enumerate() {
        if let Err(e) = perform(entry) {
            revert_all(&entries[..done]);
            return Err(e);
        }
    }
    if let Err(e) = front.check(inst) {
        // nginx never loaded the bad config, so there is nothing to reload.
        revert_all(entries);
        return Err(e);
    }
    if reload && let Err(e) = front.reload(inst) {
        revert_all(entries);
        // A partial reload may have happened; load the restored config again.
        let _ = front.reload(inst);
        return Err(e);
    }
    Ok(())
}

/// Undoes recorded entries, newest first, then checks and reloads. Files the
/// user changed since are left alone and reported as warnings. If the check
/// fails, the files are put back as they were before the undo.
///
/// # Errors
///
/// When a file cannot be written, or the check or reload fails.
pub fn undo(
    front: &dyn FrontServer,
    inst: &Installation,
    entries: &[Entry],
    reload: bool,
) -> Result<Vec<String>, FrontError> {
    let mut before = Vec::new();
    for e in entries {
        let path = match e {
            Entry::Wrote { path, .. } | Entry::Edited { path, .. } => path,
        };
        if !before.iter().any(|(p, _)| p == path) {
            before.push((path.clone(), read_opt(path)?));
        }
    }
    let put_back = |before: &[(PathBuf, Option<String>)]| {
        for (p, c) in before {
            let _ = set_contents(p, c.as_deref());
        }
    };
    let mut warnings = Vec::new();
    for e in entries.iter().rev() {
        match revert(e) {
            Ok(Some(w)) => warnings.push(w),
            Ok(None) => {}
            Err(err) => {
                put_back(&before);
                return Err(err);
            }
        }
    }
    if let Err(e) = front.check(inst) {
        put_back(&before);
        return Err(e);
    }
    if reload {
        front.reload(inst)?;
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use proxlet_core::port;

    use super::*;
    use crate::{FrontConfig, Plan};

    /// A front server whose check fails when any file under `root` contains `bogus`.
    struct FakeFront {
        root: PathBuf,
        fail_reload: bool,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FrontServer for FakeFront {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn detect(&self) -> Option<Installation> {
            None
        }
        fn is_running(&self) -> bool {
            true
        }
        fn service_unit(&self) -> Option<&'static str> {
            None
        }
        fn read(&self, _: &Installation) -> Result<FrontConfig, FrontError> {
            Ok(FrontConfig::default())
        }
        fn https_support(&self, _: &Installation) -> Result<(), String> {
            Ok(())
        }
        fn plan_attach(
            &self,
            _: &FrontConfig,
            _: u16,
            _: Option<&crate::HttpsForward>,
            _: SocketAddr,
            _: bool,
        ) -> Result<Plan, FrontError> {
            Ok(Plan::default())
        }
        fn check(&self, _: &Installation) -> Result<(), FrontError> {
            self.calls.lock().unwrap().push("check");
            for e in fs::read_dir(&self.root).unwrap().flatten() {
                if fs::read_to_string(e.path())
                    .unwrap_or_default()
                    .contains("bogus")
                {
                    return Err(FrontError::Command {
                        command: "fake -t".into(),
                        output: format!("unknown directive \"bogus\" in {}", e.path().display()),
                    });
                }
            }
            Ok(())
        }
        fn reload(&self, _: &Installation) -> Result<(), FrontError> {
            self.calls.lock().unwrap().push("reload");
            if self.fail_reload {
                return Err(FrontError::Command {
                    command: "fake -s reload".into(),
                    output: "no master".into(),
                });
            }
            Ok(())
        }
        fn error_log(&self, _: &Installation, _: &FrontConfig) -> Option<PathBuf> {
            None
        }
        fn is_bind_failure(&self, _: &str, _: u16) -> bool {
            false
        }
        fn holds(&self, _: Option<&port::Holder>, _: u16, _: Option<&FrontConfig>) -> bool {
            false
        }
    }

    struct Tree {
        dir: tempfile::TempDir,
        front: FakeFront,
    }

    fn tree(fail_reload: bool) -> Tree {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("default"),
            "server {\n    listen 80 default_server;\n    root /var/www;\n}\n",
        )
        .unwrap();
        fs::set_permissions(
            dir.path().join("default"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let front = FakeFront {
            root: dir.path().to_owned(),
            fail_reload,
            calls: Mutex::default(),
        };
        Tree { dir, front }
    }

    fn inst() -> Installation {
        Installation {
            binary: "fake".into(),
            version: "1".into(),
            conf_path: "/x".into(),
            prefix: "/".into(),
            default_error_log: None,
            ssl: true,
        }
    }

    impl Tree {
        fn p(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
        fn snapshot(&self) -> Vec<(String, String)> {
            let mut v: Vec<_> = fs::read_dir(self.dir.path())
                .unwrap()
                .flatten()
                .map(|e| {
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        fs::read_to_string(e.path()).unwrap(),
                    )
                })
                .collect();
            v.sort();
            v
        }
        fn steps(&self, block: &str) -> Vec<Step> {
            vec![
                Step::EditLine {
                    path: self.p("default"),
                    line: 2,
                    from: "    listen 80 default_server;".into(),
                    to: "    listen 80;".into(),
                },
                Step::Write {
                    path: self.p("proxlet.conf"),
                    contents: block.into(),
                },
            ]
        }
    }

    #[test]
    fn apply_then_undo_restores_byte_for_byte() {
        let t = tree(false);
        let before = t.snapshot();
        let entries = prepare(&t.steps("# Managed by proxlet.\nserver {}\n")).unwrap();
        apply(&t.front, &inst(), &entries, true).unwrap();
        assert_eq!(
            fs::read_to_string(t.p("default")).unwrap(),
            "server {\n    listen 80;\n    root /var/www;\n}\n"
        );
        assert!(t.p("proxlet.conf").exists());
        // The edited file keeps its mode.
        let mode = fs::metadata(t.p("default")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        assert_eq!(*t.front.calls.lock().unwrap(), ["check", "reload"]);

        let warnings = undo(&t.front, &inst(), &entries, true).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(t.snapshot(), before);
    }

    #[test]
    fn failed_check_restores_every_file() {
        let t = tree(false);
        let before = t.snapshot();
        let entries = prepare(&t.steps("bogus;\n")).unwrap();
        let err = apply(&t.front, &inst(), &entries, true).unwrap_err();
        assert!(
            err.to_string().contains("unknown directive \"bogus\""),
            "{err}"
        );
        assert_eq!(t.snapshot(), before);
        // No reload after a failed check.
        assert_eq!(*t.front.calls.lock().unwrap(), ["check"]);
    }

    #[test]
    fn failed_reload_restores_every_file() {
        let t = tree(true);
        let before = t.snapshot();
        let entries = prepare(&t.steps("server {}\n")).unwrap();
        assert!(apply(&t.front, &inst(), &entries, true).is_err());
        assert_eq!(t.snapshot(), before);
    }

    #[test]
    fn failed_write_restores_earlier_ones() {
        let t = tree(false);
        let before = t.snapshot();
        let mut steps = t.steps("server {}\n");
        steps.push(Step::Write {
            path: t.p("missing-dir/x.conf"),
            contents: "x".into(),
        });
        let entries = prepare(&steps).unwrap();
        assert!(apply(&t.front, &inst(), &entries, false).is_err());
        assert_eq!(t.snapshot(), before);
    }

    #[test]
    fn unchanged_writes_are_skipped() {
        let t = tree(false);
        let entries = prepare(&[Step::Write {
            path: t.p("default"),
            contents: fs::read_to_string(t.p("default")).unwrap(),
        }])
        .unwrap();
        assert_eq!(entries, []);
    }

    #[test]
    fn undo_leaves_user_edits_alone() {
        let t = tree(false);
        let entries = prepare(&t.steps("server {}\n")).unwrap();
        apply(&t.front, &inst(), &entries, false).unwrap();
        // The user rewrote both files afterwards.
        fs::write(t.p("proxlet.conf"), "server { listen 81; }\n").unwrap();
        fs::write(t.p("default"), "server {\n    listen 8080;\n}\n").unwrap();
        let warnings = undo(&t.front, &inst(), &entries, false).unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("proxlet.conf was changed"),
            "{warnings:?}"
        );
        assert!(
            warnings[1].contains("no longer reads `listen 80;`"),
            "{warnings:?}"
        );
        assert_eq!(
            fs::read_to_string(t.p("proxlet.conf")).unwrap(),
            "server { listen 81; }\n"
        );
    }

    #[test]
    fn undo_finds_a_moved_line() {
        let t = tree(false);
        let entries = prepare(&t.steps("server {}\n")).unwrap();
        apply(&t.front, &inst(), &entries, false).unwrap();
        // The user added a comment at the top; the edited line moved down.
        let text = fs::read_to_string(t.p("default")).unwrap();
        fs::write(t.p("default"), format!("# note\n{text}")).unwrap();
        assert_eq!(
            undo(&t.front, &inst(), &entries, false).unwrap(),
            Vec::<String>::new()
        );
        assert!(
            fs::read_to_string(t.p("default"))
                .unwrap()
                .contains("listen 80 default_server;")
        );
    }

    #[test]
    fn undo_whose_check_fails_puts_files_back() {
        let t = tree(false);
        let entries = prepare(&t.steps("server {}\n")).unwrap();
        apply(&t.front, &inst(), &entries, false).unwrap();
        let after_apply = t.snapshot();
        // Something unrelated broke the config meanwhile.
        fs::write(t.p("other.conf"), "bogus;\n").unwrap();
        assert!(undo(&t.front, &inst(), &entries, false).is_err());
        let mut expected = after_apply;
        expected.push(("other.conf".into(), "bogus;\n".into()));
        expected.sort();
        assert_eq!(t.snapshot(), expected);
    }

    #[test]
    fn ledger_round_trips() {
        let t = tree(false);
        let ledger = Ledger {
            front: "nginx".into(),
            port: 80,
            internal: "127.0.0.1:47080".into(),
            hooked_unit: Some("nginx.service".into()),
            https_port: Some(443),
            cert_dir: Some("/var/lib/proxlet/nginx-certs".into()),
            cert_dir_created: true,
            entries: prepare(&t.steps("server {}\n")).unwrap(),
        };
        let path = t.p("front.json");
        assert_eq!(Ledger::load(&path).unwrap(), None);
        ledger.save(&path).unwrap();
        assert_eq!(Ledger::load(&path).unwrap(), Some(ledger));
    }

    #[test]
    fn a_ledger_from_before_https_reads_as_port_80_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("front.json");
        std::fs::write(
            &path,
            r#"{"front":"nginx","port":80,"internal":"127.0.0.1:47080","entries":[]}"#,
        )
        .unwrap();
        let ledger = Ledger::load(&path).unwrap().unwrap();
        assert_eq!(ledger.https_port, None);
        assert_eq!(ledger.cert_dir, None);
        assert!(!ledger.cert_dir_created);
    }
}
