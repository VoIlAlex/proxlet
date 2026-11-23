//! The one validate → build → persist path every configuration change takes.
//!
//! The new snapshot is built before anything is written, so a change that the
//! engine would reject never reaches the file. The caller swaps the returned
//! snapshot into a running engine only after the file has been written.

use proxlet_core::{Change, ConfigError, ConfigFile, Outcome, Target};
use proxlet_server::Snapshot;

/// Why a change was rejected.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// The configuration file could not be read or written.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The change does not fit the current configuration.
    #[error(transparent)]
    Change(#[from] proxlet_core::ChangeError),
    /// The target cannot be served by this process.
    #[error(transparent)]
    Target(#[from] proxlet_core::TargetError),
    /// The engine could not use the resulting configuration.
    #[error(transparent)]
    Build(#[from] proxlet_server::BuildError),
}

/// Applies `change` to the file and returns what it did plus the new snapshot.
///
/// A failure at any step leaves the file exactly as it was.
pub fn apply(file: &ConfigFile, change: &Change) -> Result<(Outcome, Snapshot), ApplyError> {
    if let Change::AddRule(rule) = change
        && let Target::Directory(_) = rule.target
    {
        // Re-check as this process: the daemon may run as a different user than the CLI.
        rule.target.clone().validate()?;
    }
    file.transact(|doc, current| {
        let outcome = doc.apply(current, change)?;
        let next = doc.to_config()?;
        let snapshot = Snapshot::build(&next)?;
        Ok((outcome, snapshot))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxlet_core::{Route, Rule};
    use std::path::{Path, PathBuf};

    fn file() -> (tempfile::TempDir, ConfigFile) {
        let dir = tempfile::tempdir().unwrap();
        let f = ConfigFile::new(dir.path().join("proxlet.toml"));
        (dir, f)
    }

    #[test]
    fn rejected_changes_leave_file_byte_identical() {
        let (dir, f) = file();
        let add = Change::AddRule(Rule {
            route: "/api".parse().unwrap(),
            target: Target::parse("8000", Path::new("/")).unwrap(),
        });
        apply(&f, &add).unwrap();
        let before = std::fs::read(f.path()).unwrap();

        let missing_dir = Change::AddRule(Rule {
            route: "/files".parse().unwrap(),
            target: Target::Directory(dir.path().join("gone")),
        });
        assert!(matches!(
            apply(&f, &missing_dir),
            Err(ApplyError::Target(_))
        ));

        let unknown = Change::RemoveRule("/nope".parse::<Route>().unwrap());
        assert!(matches!(apply(&f, &unknown), Err(ApplyError::Change(_))));

        assert_eq!(std::fs::read(f.path()).unwrap(), before);
    }

    #[test]
    fn snapshot_reflects_the_change() {
        let (dir, f) = file();
        let change = Change::AddRule(Rule {
            route: "/files".parse().unwrap(),
            target: Target::Directory(PathBuf::from(dir.path())),
        });
        let (outcome, snapshot) = apply(&f, &change).unwrap();
        assert_eq!(outcome, Outcome::Added);
        assert_eq!(snapshot.rule_count(), 1);
    }
}
