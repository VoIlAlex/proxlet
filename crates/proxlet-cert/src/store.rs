//! Certificates on disk (design decision 2).
//!
//! ```text
//! <dir>/<domain>.pem    the chain, then the private key; mode 0600
//! <dir>/<domain>.json   which server issued it, and the renewal state
//! ```
//!
//! The chain and key share one file so a replacement is one atomic rename. The
//! dates and names always come from the PEM itself, so a stale `.json` can
//! never make proxlet believe a certificate is newer than it is.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pem::{self, Leaf};
use crate::{CertError, name};

/// Renewal state and origin of a certificate, kept next to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The ACME server that issued it: `production`, `staging` or a URL.
    pub server: String,
    /// The last renewal error, if the last attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Failed renewal attempts in a row.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failures: u32,
    /// When to try renewing again after a failure, Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt: Option<i64>,
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's signature")]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A stored certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The domain, lowercase.
    pub domain: String,
    /// Facts read from the certificate.
    pub leaf: Leaf,
    /// Origin and renewal state.
    pub meta: Meta,
    /// The PEM file (chain and key).
    pub path: PathBuf,
}

/// Everything in the store, plus files that could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Readable certificates, sorted by domain.
    pub certs: Vec<Stored>,
    /// `<file>: <why>` for each certificate file that could not be read.
    pub problems: Vec<String>,
}

/// The certificate directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// A store in `dir`, which is created on the first write.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The PEM file for `domain` (which need not exist).
    #[must_use]
    pub fn pem_path(&self, domain: &str) -> PathBuf {
        self.dir.join(format!("{domain}.pem"))
    }

    fn meta_path(&self, domain: &str) -> PathBuf {
        self.dir.join(format!("{domain}.json"))
    }

    /// Every certificate in the store.
    ///
    /// # Errors
    ///
    /// When the directory exists but cannot be read.
    pub fn list(&self) -> Result<Listing, CertError> {
        let mut listing = Listing::default();
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
            Err(e) => return Err(CertError::io(&self.dir, e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| CertError::io(&self.dir, e))?;
            let file = entry.file_name();
            let Some(domain) = file.to_str().and_then(|f| f.strip_suffix(".pem")) else {
                continue;
            };
            if domain.starts_with('.') || name::check_public(domain).as_deref() != Ok(domain) {
                continue;
            }
            match self.get(domain) {
                Ok(Some(s)) => listing.certs.push(s),
                Ok(None) => {}
                Err(e) => listing
                    .problems
                    .push(format!("{}: {e}", entry.path().display())),
            }
        }
        listing.certs.sort_by(|a, b| a.domain.cmp(&b.domain));
        Ok(listing)
    }

    /// The certificate for `domain`, if there is one.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read or parsed.
    pub fn get(&self, domain: &str) -> Result<Option<Stored>, CertError> {
        let path = self.pem_path(domain);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CertError::io(&path, e)),
        };
        let leaf = pem::leaf(&text)?;
        let meta = fs::read(self.meta_path(domain))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Ok(Some(Stored {
            domain: domain.to_owned(),
            leaf,
            meta,
            path,
        }))
    }

    /// Stores a new certificate for `domain`, replacing any old one. The chain
    /// and key are checked first; on any failure the old certificate stays.
    ///
    /// # Errors
    ///
    /// When the domain is not a valid public name, the chain and key do not
    /// match or do not cover the domain, or the file cannot be written.
    pub fn put(
        &self,
        domain: &str,
        chain_pem: &str,
        key_pem: &str,
        server: &str,
    ) -> Result<Stored, CertError> {
        let domain = name::check_public(domain).map_err(|e| CertError::Invalid(e.to_string()))?;
        let leaf = pem::validate(&domain, chain_pem, key_pem)?;
        self.ensure_dir()?;
        let mut text = chain_pem.trim_end().to_owned();
        text.push('\n');
        text.push_str(key_pem.trim_end());
        text.push('\n');
        let path = self.pem_path(&domain);
        write_atomic(&path, text.as_bytes(), 0o600)?;
        let meta = Meta {
            server: server.to_owned(),
            ..Meta::default()
        };
        self.set_meta(&domain, &meta)?;
        Ok(Stored {
            domain,
            leaf,
            meta,
            path,
        })
    }

    /// Records renewal state for `domain`.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn set_meta(&self, domain: &str, meta: &Meta) -> Result<(), CertError> {
        let json =
            serde_json::to_vec_pretty(meta).map_err(|e| CertError::Invalid(e.to_string()))?;
        write_atomic(&self.meta_path(domain), &json, 0o600)
    }

    /// Deletes the certificate for `domain`. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// When a file exists but cannot be deleted.
    pub fn remove(&self, domain: &str) -> Result<bool, CertError> {
        let pem = self.pem_path(domain);
        let existed = remove_if_exists(&pem)?;
        remove_if_exists(&self.meta_path(domain))?;
        Ok(existed)
    }

    fn ensure_dir(&self) -> Result<(), CertError> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir).map_err(|e| CertError::io(&self.dir, e))?;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| CertError::io(&self.dir, e))?;
        }
        Ok(())
    }
}

fn remove_if_exists(path: &Path) -> Result<bool, CertError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CertError::io(path, e)),
    }
}

/// Writes `bytes` to a temporary file next to `path` with `mode`, syncs it,
/// and renames it over `path`.
///
/// # Errors
///
/// When any step fails; the temporary file is removed and `path` is untouched.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), CertError> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        // `mode` only applies on creation; a leftover temp file keeps its own.
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    result.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        CertError::io(path, e)
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::fake::FakeCa;

    const DAY: i64 = 86_400;

    #[test]
    fn put_get_list_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("certs"));
        assert_eq!(store.list().unwrap(), Listing::default());

        let ca = FakeCa::new();
        let (chain, key_b) = ca.issue("b.example.com", 90 * DAY);
        let stored = store
            .put("B.example.com", &chain, &key_b, "staging")
            .unwrap();
        assert_eq!(stored.domain, "b.example.com");
        assert_eq!(stored.leaf.not_after - stored.leaf.not_before, 90 * DAY);
        assert_eq!(stored.meta.server, "staging");
        let (chain, key) = ca.issue("a.example.com", 30 * DAY);
        store
            .put("a.example.com", &chain, &key, "production")
            .unwrap();

        let listing = store.list().unwrap();
        let domains: Vec<&str> = listing.certs.iter().map(|c| c.domain.as_str()).collect();
        assert_eq!(domains, ["a.example.com", "b.example.com"]);
        assert_eq!(listing.problems, Vec::<String>::new());
        assert_eq!(store.get("b.example.com").unwrap().unwrap(), stored);

        // The file holds the chain and the key, readable only by its owner.
        let text = fs::read_to_string(&stored.path).unwrap();
        assert!(text.contains("BEGIN CERTIFICATE") && text.contains("PRIVATE KEY"));
        assert_eq!(fs::metadata(&stored.path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(store.dir()).unwrap().mode() & 0o777, 0o700);
        assert_eq!(pem::private_key(&text).unwrap().trim(), key_b.trim());

        assert!(store.remove("b.example.com").unwrap());
        assert!(!store.remove("b.example.com").unwrap());
        assert_eq!(store.list().unwrap().certs.len(), 1);
    }

    #[test]
    fn mismatched_key_is_rejected_and_the_old_certificate_stays() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let ca = FakeCa::new();
        let (chain, key) = ca.issue("app.example.com", 90 * DAY);
        let old = store
            .put("app.example.com", &chain, &key, "production")
            .unwrap();
        let before = fs::read(&old.path).unwrap();

        let (new_chain, _) = ca.issue("app.example.com", 90 * DAY);
        let (_, other_key) = ca.issue("app.example.com", 90 * DAY);
        let err = store
            .put("app.example.com", &new_chain, &other_key, "production")
            .unwrap_err();
        assert!(err.to_string().contains("does not belong"), "{err}");

        let (wrong_name, wrong_key) = ca.issue("other.example.com", 90 * DAY);
        let err = store
            .put("app.example.com", &wrong_name, &wrong_key, "production")
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("for other.example.com rather than app.example.com"),
            "{err}"
        );
        assert!(store.put("app.test", &chain, &key, "production").is_err());

        assert_eq!(fs::read(&old.path).unwrap(), before);
    }

    #[test]
    fn a_crash_mid_write_leaves_the_store_readable() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let ca = FakeCa::new();
        let (chain, key) = ca.issue("app.example.com", 90 * DAY);
        store
            .put("app.example.com", &chain, &key, "production")
            .unwrap();
        // A write that died before its rename leaves a hidden temp file.
        fs::write(dir.path().join(".app.example.com.pem.tmp-1"), "half").unwrap();
        fs::write(dir.path().join("garbage.example.com.pem"), "not a pem").unwrap();

        let listing = store.list().unwrap();
        assert_eq!(listing.certs.len(), 1);
        assert_eq!(listing.problems.len(), 1, "{:?}", listing.problems);
        assert!(listing.problems[0].contains("garbage.example.com.pem"));
    }

    #[test]
    fn renewal_state_round_trips_and_a_bad_meta_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let ca = FakeCa::new();
        let (chain, key) = ca.issue("app.example.com", 90 * DAY);
        store
            .put("app.example.com", &chain, &key, "production")
            .unwrap();
        let meta = Meta {
            server: "production".into(),
            last_error: Some("connection refused".into()),
            failures: 2,
            next_attempt: Some(1_000),
        };
        store.set_meta("app.example.com", &meta).unwrap();
        assert_eq!(store.get("app.example.com").unwrap().unwrap().meta, meta);
        fs::write(dir.path().join("app.example.com.json"), "{").unwrap();
        assert_eq!(
            store.get("app.example.com").unwrap().unwrap().meta,
            Meta::default()
        );
    }
}
