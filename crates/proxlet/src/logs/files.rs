//! The log directory: the current file, rotated files and retention
//! (design decision 5).

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use proxlet_core::Locations;
use proxlet_core::settings::MB;

/// The file being written.
pub const CURRENT: &str = "proxlet.log";

/// The size at which the current file is rotated.
pub const FILE_LIMIT: u64 = 10 * MB;

/// Lowers [`FILE_LIMIT`] in tests.
pub const FILE_LIMIT_ENV: &str = "PROXLET_TEST_LOG_FILE_BYTES";

/// The log directory.
#[must_use]
pub fn dir(locations: &Locations) -> PathBuf {
    locations.state_dir.join("logs")
}

/// [`FILE_LIMIT`], or the test override.
#[must_use]
pub fn file_limit() -> u64 {
    std::env::var(FILE_LIMIT_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(FILE_LIMIT)
}

/// How much to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// All files together, in bytes.
    pub max_size: u64,
    /// The oldest a file may be.
    pub max_age: Duration,
}

/// Creates the directory readable only by its owner.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

/// Opens the current file for appending, creating it `0600`.
pub fn open_current(dir: &Path) -> io::Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(dir.join(CURRENT))
}

/// Rotated files, oldest first, with the Unix milliseconds in their names.
pub fn rotated(dir: &Path) -> io::Result<Vec<(PathBuf, u64)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(stamp) = name
            .to_str()
            .and_then(|n| n.strip_prefix("proxlet-"))
            .and_then(|n| n.strip_suffix(".log"))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        files.push((entry.path(), stamp));
    }
    files.sort_by_key(|(_, stamp)| *stamp);
    Ok(files)
}

/// Every log file, oldest first: rotated files, then the current one.
/// A missing directory has none.
pub fn all(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = match rotated(dir) {
        Ok(r) => r.into_iter().map(|(p, _)| p).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let current = dir.join(CURRENT);
    if current.exists() {
        files.push(current);
    }
    Ok(files)
}

/// Renames the current file to its rotated name, which sorts after every
/// existing rotated file.
pub fn rotate(dir: &Path, now: SystemTime) -> io::Result<PathBuf> {
    let mut stamp = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    if let Some((_, last)) = rotated(dir)?.last() {
        stamp = stamp.max(last + 1);
    }
    let to = dir.join(format!("proxlet-{stamp}.log"));
    fs::rename(dir.join(CURRENT), &to)?;
    Ok(to)
}

/// Deletes rotated files, oldest first, while all files together exceed
/// `max_size`, and any rotated file last written before `max_age` ago. The
/// current file counts as `file_limit` bytes, the most it grows to before the
/// next rotation, so the total stays within `max_size` between runs. The
/// current file is never deleted. Returns how many were deleted.
pub fn retention(
    dir: &Path,
    limits: Limits,
    file_limit: u64,
    now: SystemTime,
) -> io::Result<usize> {
    let rotated = rotated(dir)?;
    let size = |p: &Path| fs::metadata(p).map_or(0, |m| m.len());
    let current = size(&dir.join(CURRENT)).max(file_limit);
    let mut total: u64 = rotated.iter().map(|(p, _)| size(p)).sum::<u64>() + current;
    let oldest_allowed = now.checked_sub(limits.max_age);
    let mut deleted = 0;
    for (path, _) in rotated {
        let meta = fs::metadata(&path)?;
        let too_old = oldest_allowed
            .zip(meta.modified().ok())
            .is_some_and(|(limit, modified)| modified < limit);
        if total > limits.max_size || too_old {
            fs::remove_file(&path)?;
            total = total.saturating_sub(meta.len());
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, bytes: usize) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, vec![b'x'; bytes]).unwrap();
        p
    }

    #[test]
    fn size_limit_deletes_oldest_and_never_the_current_file() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "proxlet-100.log", 400);
        write(d.path(), "proxlet-200.log", 400);
        write(d.path(), "proxlet-300.log", 400);
        write(d.path(), CURRENT, 400);
        write(d.path(), "other.txt", 9999);
        let limits = Limits {
            max_size: 1000,
            max_age: Duration::from_secs(86_400),
        };
        assert_eq!(
            retention(d.path(), limits, 400, SystemTime::now()).unwrap(),
            2
        );
        let left: Vec<String> = all(d.path())
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["proxlet-300.log", CURRENT]);

        // Even a current file over the limit stays.
        let tiny = Limits {
            max_size: 1,
            ..limits
        };
        retention(d.path(), tiny, 400, SystemTime::now()).unwrap();
        assert!(d.path().join(CURRENT).exists());
    }

    #[test]
    fn age_limit() {
        let d = tempfile::tempdir().unwrap();
        let old = write(d.path(), "proxlet-100.log", 10);
        let new = write(d.path(), "proxlet-200.log", 10);
        let two_days = SystemTime::now() - Duration::from_secs(2 * 86_400);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(two_days)
            .unwrap();
        let limits = Limits {
            max_size: MB,
            max_age: Duration::from_secs(86_400),
        };
        assert_eq!(
            retention(d.path(), limits, 400, SystemTime::now()).unwrap(),
            1
        );
        assert!(!old.exists() && new.exists());
    }

    #[test]
    fn rotation_names_sort_in_time_order() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "proxlet-5000.log", 1);
        write(d.path(), CURRENT, 1);
        let at = UNIX_EPOCH + Duration::from_millis(1000);
        let to = rotate(d.path(), at).unwrap();
        assert_eq!(to.file_name().unwrap(), "proxlet-5001.log");
        assert!(!d.path().join(CURRENT).exists());
    }

    #[test]
    fn missing_directory_has_no_files() {
        assert_eq!(
            all(Path::new("/nonexistent/proxlet/logs")).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn private_directory_and_file() {
        let d = tempfile::tempdir().unwrap();
        let logs = d.path().join("logs");
        create_dir(&logs).unwrap();
        open_current(&logs).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&logs), 0o700);
        assert_eq!(mode(&logs.join(CURRENT)), 0o600);
    }
}
