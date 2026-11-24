//! Editing a tool's JSON configuration in place: other keys keep their order,
//! the previous file is backed up, and the new one replaces it atomically.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::AiError;

/// The backup written next to `path` before it is changed.
#[must_use]
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".proxlet-backup");
    path.with_file_name(name)
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> AiError + '_ {
    move |source| AiError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A JSON object file as read: its bytes (if it existed) and its contents.
pub struct Doc {
    pub previous: Option<Vec<u8>>,
    pub root: Map<String, Value>,
}

/// Reads `path` as a JSON object. A missing or blank file is an empty object.
pub fn read(path: &Path) -> Result<Doc, AiError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Doc {
                previous: None,
                root: Map::new(),
            });
        }
        Err(e) => return Err(io(path)(e)),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Doc {
            previous: Some(bytes),
            root: Map::new(),
        });
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(root)) => Ok(Doc {
            previous: Some(bytes),
            root,
        }),
        Ok(_) => Err(AiError::Parse {
            path: path.to_path_buf(),
            message: "the top level is not an object".into(),
        }),
        Err(e) => Err(AiError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        }),
    }
}

/// Writes `doc.root` to `path`: backs up the previous file, then writes a
/// temporary file in the same directory (with the old file's mode, or 0600)
/// and renames it over `path`.
pub fn write(path: &Path, doc: &Doc) -> Result<(), AiError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    if let Some(previous) = &doc.previous {
        let backup = backup_path(path);
        std::fs::write(&backup, previous).map_err(io(&backup))?;
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(mode))
            .map_err(io(&backup))?;
    }
    let mut text = serde_json::to_string_pretty(&doc.root).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".proxlet-{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(io(path))
}
