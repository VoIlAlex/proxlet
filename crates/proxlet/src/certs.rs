//! Certificates in the running server: loading them into the TLS resolver,
//! issuing and renewing through ACME (the daemon answers the challenge), and
//! what the redirect to HTTPS is based on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxlet_cert::acme::{self, AcmeCa, AcmeError, Ca, ChallengeSink, Http};
use proxlet_cert::fake::{FakeAcme, FakeCa};
use proxlet_cert::{Store, Stored, renew};
use proxlet_core::settings::AcmeServer;
use proxlet_server::tls::{CertMap, CertResolver, certified_key};
use proxlet_server::{Engine, TlsView};
use tokio::sync::watch;

use crate::ports::Mode;

/// Test hook: issue from a local fake CA instead of the configured ACME
/// server. The value is the certificates' lifetime in seconds. The fake CA's
/// root is written to [`FAKE_ROOT`] in the state directory.
pub const FAKE_ACME_ENV: &str = "PROXLET_TEST_FAKE_ACME";
/// Test hook: the fake CA fails every order with this message.
pub const FAKE_ACME_FAIL_ENV: &str = "PROXLET_TEST_FAKE_ACME_FAIL";
/// Test hook: run the renewal check this often (milliseconds), from startup.
pub const RENEW_EVERY_ENV: &str = "PROXLET_TEST_RENEW_EVERY_MS";
/// The fake CA's root certificate, in the state directory.
pub const FAKE_ROOT: &str = "fake-acme-root.pem";

/// How long a reachability probe token is answered.
pub const PROBE_TTL: Duration = Duration::from_secs(30);

/// Serves challenges from the engine.
struct EngineSink(Arc<Engine>);

impl ChallengeSink for EngineSink {
    fn publish(&self, token: &str, key_authorization: &str) {
        self.0.publish_challenge(token, key_authorization);
    }
    fn withdraw(&self, token: &str) {
        self.0.withdraw_challenge(token);
    }
}

/// The server's certificates.
pub struct Certs {
    store: Store,
    acme_dir: PathBuf,
    resolver: Arc<CertResolver>,
    engine: Arc<Engine>,
    https_mode: watch::Receiver<Mode>,
    https_port: u16,
    ledger: PathBuf,
    order: tokio::sync::Mutex<()>,
    cas: Mutex<HashMap<String, Arc<dyn Ca>>>,
    fake: Option<Arc<FakeAcme>>,
}

impl std::fmt::Debug for Certs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Certs")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

/// The result of renewing one certificate.
#[derive(Debug)]
pub struct Renewed {
    /// The domain.
    pub domain: String,
    /// The new certificate, or why there is none.
    pub result: Result<Stored, String>,
}

impl Certs {
    /// Certificates stored under `state_dir`. The attach ledger there says
    /// whether a front server holding the HTTPS port forwards it to proxlet,
    /// and where to publish certificates for it.
    ///
    /// # Panics
    ///
    /// If the fake CA test hook is set and its root cannot be written.
    #[must_use]
    pub fn new(
        state_dir: &Path,
        resolver: Arc<CertResolver>,
        engine: Arc<Engine>,
        https_mode: watch::Receiver<Mode>,
        https_port: u16,
    ) -> Self {
        let fake = std::env::var(FAKE_ACME_ENV).ok().map(|secs| {
            let acme = FakeAcme::new(FakeCa::new()).with_terms("https://fake-acme.invalid/terms");
            acme.set_validity(secs.parse().unwrap_or(90 * 86_400));
            if let Ok(why) = std::env::var(FAKE_ACME_FAIL_ENV) {
                acme.fail_with(&why);
            }
            std::fs::create_dir_all(state_dir).expect("create the state directory");
            std::fs::write(state_dir.join(FAKE_ROOT), acme.ca().root_pem())
                .expect("write the fake CA root");
            Arc::new(acme)
        });
        Self {
            store: Store::new(state_dir.join("certs")),
            acme_dir: state_dir.join("acme"),
            resolver,
            engine,
            https_mode,
            https_port,
            ledger: state_dir.join(crate::attach::LEDGER),
            order: tokio::sync::Mutex::new(()),
            cas: Mutex::new(HashMap::new()),
            fake,
        }
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Loads every stored certificate into the resolver and refreshes the
    /// redirect view. Returns how many were loaded.
    pub fn reload(&self) -> usize {
        let listing = match self.store.list() {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("cannot read certificates: {e}");
                return self.resolver.names().len();
            }
        };
        for p in &listing.problems {
            tracing::warn!("skipping a certificate: {p}");
        }
        let mut map = CertMap::new();
        for cert in &listing.certs {
            match std::fs::read(&cert.path)
                .map_err(|e| e.to_string())
                .and_then(|pem| certified_key(&pem))
            {
                Ok(key) => {
                    map.insert(cert.domain.clone(), Arc::new(key));
                }
                Err(e) => tracing::warn!("skipping {}: {e}", cert.path.display()),
            }
        }
        let count = map.len();
        self.resolver.replace(map);
        self.refresh_view();
        self.publish(&listing.certs);
        count
    }

    /// Copies every certificate to the directory the front server reads them
    /// from, if that is not the store itself, and deletes copies of removed
    /// certificates (design decision 8).
    fn publish(&self, certs: &[Stored]) {
        let Some(dir) = crate::attach::published_cert_dir(&self.ledger, self.store.dir()) else {
            return;
        };
        if !dir.is_dir() {
            tracing::warn!(
                "{} does not exist; nginx cannot read proxlet's certificates until `sudo proxlet start` creates it",
                dir.display()
            );
            return;
        }
        for cert in certs {
            let target = dir.join(format!("{}.pem", cert.domain));
            let result = std::fs::read(&cert.path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    proxlet_cert::store::write_atomic(&target, &bytes, 0o640)
                        .map_err(|e| e.to_string())
                });
            if let Err(e) = result {
                tracing::warn!("cannot publish {} for nginx: {e}", cert.domain);
            }
        }
        unpublish_stale(&dir, certs);
    }

    /// Whether HTTPS is being served right now (design decision 4).
    #[must_use]
    pub fn https_served(&self) -> bool {
        match &*self.https_mode.borrow() {
            Mode::Standalone => true,
            Mode::Attached { .. } => crate::attach::forwards_https(&self.ledger),
            Mode::Waiting { .. } | Mode::Released => false,
        }
    }

    /// Rebuilds what the redirect is based on, after the certificates or the
    /// HTTPS port's mode changed.
    pub fn refresh_view(&self) {
        self.engine.set_tls_view(TlsView {
            names: self.resolver.names().into_iter().collect(),
            https_served: self.https_served(),
            https_port: self.https_port,
        });
    }

    /// Answers a random token for [`PROBE_TTL`] so the CLI can check that a
    /// domain reaches this server. Returns `(token, body)`.
    #[must_use]
    pub fn probe(&self) -> (String, String) {
        let token = random_token();
        let body = format!("proxlet-probe-{token}");
        self.engine.publish_challenge(&token, &body);
        let engine = self.engine.clone();
        let t = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(PROBE_TTL).await;
            engine.withdraw_challenge(&t);
        });
        (token, body)
    }

    fn ca_for(&self, server: &AcmeServer) -> Result<Arc<dyn Ca>, AcmeError> {
        if let Some(fake) = &self.fake {
            return Ok(fake.clone());
        }
        let id = acme::server_id(server);
        let mut cas = self.cas.lock().expect("CA cache lock");
        if let Some(ca) = cas.get(&id) {
            return Ok(ca.clone());
        }
        let ca: Arc<dyn Ca> = Arc::new(AcmeCa::new(server, &self.acme_dir, Http::from_env()?));
        cas.insert(id, ca.clone());
        Ok(ca)
    }

    /// Gets a certificate for `domain` from the configured ACME server.
    ///
    /// # Errors
    ///
    /// When the ACME server refuses, cannot be reached, or the terms are needed.
    pub async fn issue(&self, domain: &str, accept_terms: bool) -> Result<Stored, AcmeError> {
        let _order = self.order.lock().await;
        let server = self.engine.snapshot().settings().cert_server.clone();
        let ca = self.ca_for(&server)?;
        let sink = EngineSink(self.engine.clone());
        let stored = acme::obtain(
            &*ca,
            &self.store,
            domain,
            &server.to_string(),
            &sink,
            accept_terms,
        )
        .await?;
        self.reload();
        tracing::info!(
            "certificate for {domain} issued, valid until {}",
            proxlet_cert::date(stored.leaf.not_after)
        );
        Ok(stored)
    }

    /// Renews `domains` (or every certificate when empty). With `force`, a
    /// certificate is renewed even when it is not due. Failures are recorded
    /// in the store so `cert ls` and `status` can show them.
    pub async fn renew(&self, domains: &[String], force: bool) -> Vec<Renewed> {
        let listing = match self.store.list() {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("cannot read certificates: {e}");
                return Vec::new();
            }
        };
        let now = proxlet_cert::now();
        let server = self.engine.snapshot().settings().cert_server.to_string();
        let mut out = Vec::new();
        for cert in listing.certs {
            if !domains.is_empty() && !domains.contains(&cert.domain) {
                continue;
            }
            if !force && !renew::should_renew(&cert, now) {
                continue;
            }
            // Its terms were accepted when this certificate was first issued;
            // a different server's terms need `proxlet cert` again.
            let accepted = cert.meta.server == server;
            let result = match self.issue(&cert.domain, accepted).await {
                Ok(s) => Ok(s),
                Err(e) => {
                    let why = match e {
                        AcmeError::TermsNeeded { .. } => format!(
                            "{server} needs its terms accepted first; run: proxlet cert {}",
                            cert.domain
                        ),
                        e => e.to_string(),
                    };
                    tracing::error!("renewing the certificate for {} failed: {why}", cert.domain);
                    let meta = renew::after_failure(&cert.meta, &why, proxlet_cert::now());
                    if let Err(e) = self.store.set_meta(&cert.domain, &meta) {
                        tracing::warn!("cannot record the renewal failure: {e}");
                    }
                    Err(why)
                }
            };
            out.push(Renewed {
                domain: cert.domain,
                result,
            });
        }
        out
    }

    /// The number of certificates and a warning for each one that expires
    /// within [`renew::WARN_DAYS`] or whose renewal is failing.
    #[must_use]
    pub fn summary(&self) -> (usize, Vec<String>) {
        let Ok(listing) = self.store.list() else {
            return (0, Vec::new());
        };
        let now = proxlet_cert::now();
        let mut warnings = Vec::new();
        for c in &listing.certs {
            let days = proxlet_cert::days_left(c.leaf.not_after, now);
            let failing = c.meta.last_error.as_deref();
            if days <= renew::WARN_DAYS || failing.is_some() {
                let when = if days < 0 {
                    "has expired".to_owned()
                } else {
                    format!("expires in {days} day{}", if days == 1 { "" } else { "s" })
                };
                let why = failing
                    .map(|e| format!("; renewal is failing: {e}"))
                    .unwrap_or_default();
                warnings.push(format!("the certificate for {} {when}{why}", c.domain));
            }
        }
        (listing.certs.len(), warnings)
    }
}

/// Deletes published `<domain>.pem` files whose certificate is gone.
fn unpublish_stale(dir: &Path, certs: &[Stored]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(domain) = name.to_str().and_then(|n| n.strip_suffix(".pem")) else {
            continue;
        };
        if proxlet_cert::check_public(domain).as_deref() == Ok(domain)
            && !certs.iter().any(|c| c.domain == domain)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Deletes the published copy of `domain`'s certificate, for `cert rm` while
/// the server is stopped.
pub fn unpublish(state_dir: &Path, domain: &str) {
    let ledger = state_dir.join(crate::attach::LEDGER);
    let store = state_dir.join("certs");
    if let Some(dir) = crate::attach::published_cert_dir(&ledger, &store) {
        let _ = std::fs::remove_file(dir.join(format!("{domain}.pem")));
    }
}

/// Renews due certificates every `every`, starting after `first`.
pub async fn renewal_loop(
    certs: Arc<Certs>,
    first: Duration,
    every: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let mut wait = first;
    loop {
        tokio::select! {
            _ = stop.wait_for(|s| *s) => return,
            () = tokio::time::sleep(wait) => {}
        }
        for r in certs.renew(&[], false).await {
            if r.result.is_ok() {
                tracing::info!("renewed the certificate for {}", r.domain);
            }
        }
        wait = every;
    }
}

/// When the renewal loop first runs and how often after that.
#[must_use]
pub fn renewal_schedule() -> (Duration, Duration) {
    if let Some(ms) = std::env::var(RENEW_EVERY_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        let every = Duration::from_millis(ms);
        return (every, every);
    }
    // Spread restarts out so many servers don't hit the CA together.
    let jitter = u64::from(std::process::id()) % 300;
    (Duration::from_secs(jitter), renew::CHECK_EVERY)
}

/// A random hex token for probes.
fn random_token() -> String {
    use std::io::Read as _;
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    })
}
