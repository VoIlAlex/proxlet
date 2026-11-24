//! Reading the log files back: history and following (design decision 6).

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::files::{self, CURRENT};

/// Every line of every file, oldest first, passed to `each` as it is read.
///
/// # Errors
///
/// When a file cannot be read.
pub fn each_line(dir: &Path, mut each: impl FnMut(&str)) -> io::Result<()> {
    for path in files::all(dir)? {
        let file = match File::open(&path) {
            Ok(f) => f,
            // Deleted by retention since it was listed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for line in BufReader::new(file).split(b'\n') {
            if let Ok(text) = String::from_utf8(line?) {
                each(&text);
            }
        }
    }
    Ok(())
}

/// The last `n` lines for which `keep` holds, oldest first, read from
/// `first` (the current file's content) and then the rotated files, newest
/// first, until there are enough.
///
/// # Errors
///
/// When a rotated file cannot be read.
pub fn last_matching(
    dir: &Path,
    first: Vec<String>,
    n: usize,
    keep: impl Fn(&str) -> bool,
) -> io::Result<Vec<String>> {
    let mut found: Vec<String> = first.into_iter().filter(|l| keep(l)).collect();
    if found.len() < n
        && let Ok(rotated) = files::rotated(dir)
    {
        for (path, _) in rotated.into_iter().rev() {
            let text = match fs::read(&path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let mut older: Vec<String> = text
                .lines()
                .filter(|l| keep(l))
                .map(str::to_owned)
                .collect();
            older.append(&mut found);
            found = older;
            if found.len() >= n {
                break;
            }
        }
    }
    let skip = found.len().saturating_sub(n);
    Ok(found.split_off(skip))
}

/// Follows the current log file across rotations: it keeps the open file,
/// reads it to its end when a new `proxlet.log` appears (a different inode),
/// reads any files rotated since, then continues with the new one from its
/// start, so nothing is lost or read twice.
#[derive(Debug)]
pub struct Follower {
    path: PathBuf,
    file: Option<File>,
    inode: u64,
    read: u64,
    partial: Vec<u8>,
}

impl Follower {
    /// Starts at the beginning of the current file, if there is one.
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        let mut f = Self {
            path: dir.join(CURRENT),
            file: None,
            inode: 0,
            read: 0,
            partial: Vec::new(),
        };
        f.open();
        f
    }

    fn open(&mut self) {
        if let Ok(file) = File::open(&self.path)
            && let Ok(meta) = file.metadata()
        {
            self.inode = meta.ino();
            self.read = 0;
            self.file = Some(file);
        }
    }

    /// Complete lines written since the last call.
    ///
    /// # Errors
    ///
    /// When the open file cannot be read.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();
        if self.file.is_none() {
            self.open();
        }
        self.drain(&mut lines)?;
        match fs::metadata(&self.path) {
            Ok(meta) if self.file.is_some() && meta.ino() != self.inode => {
                // Rotated: the old file is complete; finish it, read the
                // files rotated after it, then switch.
                self.drain(&mut lines)?;
                self.partial.clear();
                self.read_rotated_after(&mut lines)?;
                self.open();
                self.drain(&mut lines)?;
            }
            Ok(meta) if meta.len() < self.read => {
                // Truncated in place.
                self.partial.clear();
                self.open();
                self.drain(&mut lines)?;
            }
            _ => {}
        }
        Ok(lines)
    }

    /// Reads, whole, the rotated files newer than the one open now. When it
    /// was deleted meanwhile, which files are newer is unknown, and none are read.
    fn read_rotated_after(&self, lines: &mut Vec<String>) -> io::Result<()> {
        let Some(dir) = self.path.parent() else {
            return Ok(());
        };
        let rotated = files::rotated(dir)?;
        let Some(at) = rotated
            .iter()
            .position(|(p, _)| fs::metadata(p).is_ok_and(|m| m.ino() == self.inode))
        else {
            return Ok(());
        };
        for (path, _) in &rotated[at + 1..] {
            match fs::read(path) {
                Ok(bytes) => {
                    lines.extend(String::from_utf8_lossy(&bytes).lines().map(str::to_owned));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn drain(&mut self, lines: &mut Vec<String>) -> io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let mut buf = Vec::new();
        let n = file.read_to_end(&mut buf)?;
        self.read += n as u64;
        self.partial.extend_from_slice(&buf);
        while let Some(i) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            lines.push(String::from_utf8_lossy(&line[..i]).into_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn append(dir: &Path, text: &str) {
        let mut f = files::open_current(dir).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn follows_across_rotation_without_loss_or_repeat() {
        let d = tempfile::tempdir().unwrap();
        let mut f = Follower::new(d.path());
        assert_eq!(f.poll().unwrap(), Vec::<String>::new());
        append(d.path(), "1\n2\n3");
        assert_eq!(f.poll().unwrap(), ["1", "2"]);
        append(d.path(), "\n4\n");
        files::rotate(d.path(), UNIX_EPOCH + Duration::from_secs(1)).unwrap();
        append(d.path(), "5\n");
        assert_eq!(f.poll().unwrap(), ["3", "4", "5"]);
        append(d.path(), "6\n");
        assert_eq!(f.poll().unwrap(), ["6"]);
    }

    #[test]
    fn several_rotations_between_polls() {
        let d = tempfile::tempdir().unwrap();
        let mut f = Follower::new(d.path());
        append(d.path(), "1\n");
        assert_eq!(f.poll().unwrap(), ["1"]);
        append(d.path(), "2\n");
        for (i, text) in ["3\n", "4\n5\n"].iter().enumerate() {
            files::rotate(d.path(), UNIX_EPOCH + Duration::from_secs(i as u64)).unwrap();
            append(d.path(), text);
        }
        files::rotate(d.path(), UNIX_EPOCH + Duration::from_secs(9)).unwrap();
        append(d.path(), "6\n");
        assert_eq!(f.poll().unwrap(), ["2", "3", "4", "5", "6"]);
    }

    #[test]
    fn rotation_before_the_new_file_exists() {
        let d = tempfile::tempdir().unwrap();
        append(d.path(), "1\n");
        let mut f = Follower::new(d.path());
        assert_eq!(f.poll().unwrap(), ["1"]);
        append(d.path(), "2\n");
        files::rotate(d.path(), UNIX_EPOCH).unwrap();
        assert_eq!(f.poll().unwrap(), ["2"]);
        append(d.path(), "3\n");
        assert_eq!(f.poll().unwrap(), ["3"]);
    }

    #[test]
    fn history_spans_files() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("proxlet-1.log"), "a1\nb2\na3\n").unwrap();
        fs::write(d.path().join("proxlet-2.log"), "a4\nb5\n").unwrap();
        let current = vec!["a6".to_owned(), "b7".to_owned()];
        let starts_a = |l: &str| l.starts_with('a');
        assert_eq!(
            last_matching(d.path(), current.clone(), 2, starts_a).unwrap(),
            ["a4", "a6"]
        );
        assert_eq!(
            last_matching(d.path(), current, 20, starts_a).unwrap(),
            ["a1", "a3", "a4", "a6"]
        );
        let mut all = Vec::new();
        each_line(d.path(), |l| all.push(l.to_owned())).unwrap();
        assert_eq!(all, ["a1", "b2", "a3", "a4", "b5"]);
    }
}
