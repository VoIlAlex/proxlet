//! Getting certificates from an ACME server (design decision 1).
//!
//! [`obtain`] runs the whole flow for one domain against any [`Ca`]: make sure
//! there is an account, create a key and CSR, have the CA validate the domain
//! through the HTTP-01 challenge (answered by whatever implements
//! [`ChallengeSink`], normally the running server), and store the result.
//! [`AcmeCa`] is the real implementation on top of `instant-acme`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt as _;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, BodyWrapper, BytesResponse, ChallengeType,
    HttpClient, Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use proxlet_core::settings::AcmeServer;
use rcgen::{CertificateParams, DistinguishedName, KeyPair};

use crate::store::write_atomic;
use crate::{CertError, Store, Stored};

/// Extra trusted roots for the ACME server's own TLS certificate, as a PEM
/// file (for Pebble and other test servers).
pub const ROOTS_ENV: &str = "PROXLET_ACME_ROOTS";

/// The User-Agent proxlet sends to ACME servers.
pub const USER_AGENT: &str = concat!("proxlet/", env!("CARGO_PKG_VERSION"));

/// How long to wait for the CA to validate a challenge or issue the chain.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(90);

/// A boxed future, so [`Ca`] can be used as a trait object.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Answers HTTP-01 challenges: serves `key_authorization` at
/// `/.well-known/acme-challenge/<token>` while it is published.
pub trait ChallengeSink: Send + Sync {
    /// Starts answering `token`.
    fn publish(&self, token: &str, key_authorization: &str);
    /// Stops answering `token`.
    fn withdraw(&self, token: &str);
}

/// Why getting a certificate failed.
#[derive(Debug, thiserror::Error)]
pub enum AcmeError {
    /// There is no account yet and the terms have not been accepted.
    #[error("the ACME server's terms of service must be accepted first")]
    TermsNeeded {
        /// The terms, if the server publishes a URL.
        url: Option<String>,
    },
    /// The CA refused or failed the order; the message is the CA's own.
    #[error("{0}")]
    Refused(String),
    /// The CA could not be reached.
    #[error("could not reach the ACME server: {0}")]
    Network(String),
    /// The CA did not finish in time.
    #[error("the ACME server did not finish within {} seconds", POLL_TIMEOUT.as_secs())]
    Timeout,
    /// Local state could not be read or written.
    #[error(transparent)]
    Store(#[from] CertError),
}

impl From<instant_acme::Error> for AcmeError {
    fn from(e: instant_acme::Error) -> Self {
        match e {
            instant_acme::Error::Api(p) => Self::Refused(problem_text(&p)),
            instant_acme::Error::Timeout(_) => Self::Timeout,
            instant_acme::Error::Other(inner) => Self::Network(inner.to_string()),
            other => Self::Network(other.to_string()),
        }
    }
}

/// The CA's explanation, without `instant-acme`'s "API error" prefix.
fn problem_text(p: &instant_acme::Problem) -> String {
    let mut text = p
        .detail
        .clone()
        .unwrap_or_else(|| "the request was refused".into());
    for sub in &p.subproblems {
        if let Some(d) = &sub.detail {
            text.push_str("; ");
            text.push_str(d);
        }
    }
    text
}

/// A certificate authority that can issue for one domain at a time.
pub trait Ca: Send + Sync {
    /// Makes sure an account exists, creating one when `accept_terms` is set.
    fn ensure_account(&self, accept_terms: bool) -> BoxFut<'_, Result<(), AcmeError>>;

    /// Validates `domain` through HTTP-01 (answered by `sink`) and returns the
    /// chain PEM for `csr_der`.
    fn certify<'a>(
        &'a self,
        domain: &'a str,
        csr_der: &'a [u8],
        sink: &'a dyn ChallengeSink,
    ) -> BoxFut<'a, Result<String, AcmeError>>;
}

/// Gets a certificate for `domain` from `ca` and stores it, recording `server`
/// as its origin.
///
/// # Errors
///
/// When the account, the order or storing fails; the old certificate, if any,
/// stays in place.
pub async fn obtain(
    ca: &dyn Ca,
    store: &Store,
    domain: &str,
    server: &str,
    sink: &dyn ChallengeSink,
    accept_terms: bool,
) -> Result<Stored, AcmeError> {
    ca.ensure_account(accept_terms).await?;
    let key = KeyPair::generate().map_err(|e| CertError::Invalid(e.to_string()))?;
    let mut params = CertificateParams::new(vec![domain.to_owned()])
        .map_err(|e| CertError::Invalid(e.to_string()))?;
    params.distinguished_name = DistinguishedName::new();
    let csr = params
        .serialize_request(&key)
        .map_err(|e| CertError::Invalid(e.to_string()))?;
    let chain = ca.certify(domain, csr.der(), sink).await?;
    Ok(store.put(domain, &chain, &key.serialize_pem(), server)?)
}

/// The directory name for a server's account: `production`, `staging`, or
/// `custom-<hash>` for a URL.
#[must_use]
pub fn server_id(server: &AcmeServer) -> String {
    match server {
        AcmeServer::Production => "production".into(),
        AcmeServer::Staging => "staging".into(),
        AcmeServer::Url(url) => {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in url.bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            format!("custom-{h:016x}")
        }
    }
}

/// The HTTPS client used for the ACME server: Mozilla's roots plus, for test
/// servers, the roots in [`ROOTS_ENV`].
#[derive(Clone)]
pub struct Http(Client<HttpsConnector<HttpConnector>, BodyWrapper<Bytes>>);

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Http")
    }
}

impl Http {
    /// A client trusting Mozilla's roots and every certificate in `extra_roots`.
    ///
    /// # Errors
    ///
    /// When `extra_roots` cannot be read or holds no usable certificate.
    pub fn new(extra_roots: Option<&Path>) -> Result<Self, AcmeError> {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject as _;

        let mut roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        if let Some(path) = extra_roots {
            let certs = CertificateDer::pem_file_iter(path)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .map_err(|e| {
                    AcmeError::Store(CertError::Invalid(format!(
                        "{} ({ROOTS_ENV}): {e}",
                        path.display()
                    )))
                })?;
            let (added, _) = roots.add_parsable_certificates(certs);
            if added == 0 {
                return Err(AcmeError::Store(CertError::Invalid(format!(
                    "{} ({ROOTS_ENV}) holds no usable certificate",
                    path.display()
                ))));
            }
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| AcmeError::Network(e.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_only()
            .enable_http1()
            .enable_http2()
            .build();
        Ok(Self(Client::builder(TokioExecutor::new()).build(connector)))
    }

    /// A client from the environment: [`ROOTS_ENV`] adds roots when set.
    ///
    /// # Errors
    ///
    /// See [`Http::new`].
    pub fn from_env() -> Result<Self, AcmeError> {
        match std::env::var_os(ROOTS_ENV).filter(|v| !v.is_empty()) {
            Some(p) => Self::new(Some(Path::new(&p))),
            None => Self::new(None),
        }
    }

    /// Fetches `url` and parses it as JSON.
    async fn get_json(&self, url: &str) -> Result<serde_json::Value, AcmeError> {
        // ACME servers may refuse requests without a User-Agent (RFC 8555, 6.1).
        let req = http::Request::get(url)
            .header(http::header::USER_AGENT, USER_AGENT)
            .body(BodyWrapper::default())
            .map_err(|e| AcmeError::Network(e.to_string()))?;
        let rsp = tokio::time::timeout(Duration::from_secs(30), self.0.request(req))
            .await
            .map_err(|_| AcmeError::Network(format!("{url} did not answer within 30 seconds")))?
            .map_err(|e| AcmeError::Network(format!("{url}: {e}")))?;
        let status = rsp.status();
        let body = rsp
            .into_body()
            .collect()
            .await
            .map_err(|e| AcmeError::Network(e.to_string()))?
            .to_bytes();
        if !status.is_success() {
            return Err(AcmeError::Network(format!("{url} answered {status}")));
        }
        serde_json::from_slice(&body)
            .map_err(|e| AcmeError::Network(format!("{url} is not an ACME directory: {e}")))
    }
}

impl HttpClient for Http {
    fn request(
        &self,
        req: http::Request<BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, instant_acme::Error>> + Send>> {
        let fut = self.0.request(req);
        Box::pin(async move {
            fut.await
                .map(BytesResponse::from)
                .map_err(|e| instant_acme::Error::Other(Box::new(e)))
        })
    }
}

/// A real ACME server, with its account stored in `account_dir`.
pub struct AcmeCa {
    directory: String,
    account_file: PathBuf,
    http: Http,
    account: tokio::sync::Mutex<Option<Account>>,
}

impl std::fmt::Debug for AcmeCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeCa")
            .field("directory", &self.directory)
            .field("account_file", &self.account_file)
            .finish_non_exhaustive()
    }
}

impl AcmeCa {
    /// The CA for `server`, keeping its account under `acme_dir/<server-id>/`.
    #[must_use]
    pub fn new(server: &AcmeServer, acme_dir: &Path, http: Http) -> Self {
        Self {
            directory: server.directory_url().to_owned(),
            account_file: acme_dir.join(server_id(server)).join("account.json"),
            http,
            account: tokio::sync::Mutex::new(None),
        }
    }

    /// The terms-of-service URL the server's directory publishes, if any.
    ///
    /// # Errors
    ///
    /// When the directory cannot be fetched.
    pub async fn terms_url(&self) -> Result<Option<String>, AcmeError> {
        let dir = self.http.get_json(&self.directory).await?;
        Ok(dir
            .pointer("/meta/termsOfService")
            .and_then(|v| v.as_str())
            .map(str::to_owned))
    }

    async fn load_or_create(&self, accept_terms: bool) -> Result<Account, AcmeError> {
        let builder = || Account::builder_with_http(Box::new(self.http.clone()));
        if let Ok(bytes) = std::fs::read(&self.account_file) {
            let creds: AccountCredentials = serde_json::from_slice(&bytes)
                .map_err(|e| CertError::Invalid(format!("{}: {e}", self.account_file.display())))?;
            return Ok(builder().from_credentials(creds).await?);
        }
        if !accept_terms {
            return Err(AcmeError::TermsNeeded {
                url: self.terms_url().await?,
            });
        }
        let (account, creds) = builder()
            .create(
                &NewAccount {
                    contact: &[],
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                self.directory.clone(),
                None,
            )
            .await?;
        let json = serde_json::to_vec(&creds).map_err(|e| CertError::Invalid(e.to_string()))?;
        if let Some(dir) = self.account_file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| CertError::io(dir, e))?;
        }
        write_atomic(&self.account_file, &json, 0o600)?;
        Ok(account)
    }

    async fn run_order(
        &self,
        account: &Account,
        domain: &str,
        csr_der: &[u8],
        sink: &dyn ChallengeSink,
        published: &mut Vec<String>,
    ) -> Result<String, AcmeError> {
        let ids = [Identifier::Dns(domain.to_owned())];
        let mut order = account.new_order(&NewOrder::new(&ids)).await?;
        let mut authorizations = order.authorizations();
        while let Some(authz) = authorizations.next().await {
            let mut authz = authz?;
            if authz.status != AuthorizationStatus::Pending {
                continue;
            }
            let mut challenge = authz.challenge(ChallengeType::Http01).ok_or_else(|| {
                AcmeError::Refused(format!(
                    "the ACME server offers no HTTP-01 challenge for {domain}"
                ))
            })?;
            let key_auth = challenge.key_authorization();
            sink.publish(&challenge.token, key_auth.as_str());
            published.push(challenge.token.clone());
            challenge.set_ready().await?;
        }
        let retry = RetryPolicy::new().timeout(POLL_TIMEOUT);
        match order.poll_ready(&retry).await? {
            OrderStatus::Ready => {}
            OrderStatus::Invalid => return Err(self.why_invalid(&mut order, domain).await),
            other => {
                return Err(AcmeError::Refused(format!(
                    "the order for {domain} ended up {other:?}"
                )));
            }
        }
        order.finalize_csr(csr_der).await?;
        Ok(order.poll_certificate(&retry).await?)
    }

    /// The CA's reason for a failed authorization, e.g. the challenge error.
    async fn why_invalid(&self, order: &mut instant_acme::Order, domain: &str) -> AcmeError {
        let mut authorizations = order.authorizations();
        while let Some(Ok(mut authz)) = authorizations.next().await {
            if let Ok(state) = authz.refresh().await
                && let Some(p) = state.challenges.iter().find_map(|c| c.error.as_ref())
            {
                return AcmeError::Refused(format!(
                    "the ACME server could not validate {domain}: {}",
                    problem_text(p)
                ));
            }
        }
        AcmeError::Refused(format!("the ACME server could not validate {domain}"))
    }
}

impl Ca for AcmeCa {
    fn ensure_account(&self, accept_terms: bool) -> BoxFut<'_, Result<(), AcmeError>> {
        Box::pin(async move {
            let mut slot = self.account.lock().await;
            if slot.is_none() {
                *slot = Some(self.load_or_create(accept_terms).await?);
            }
            Ok(())
        })
    }

    fn certify<'a>(
        &'a self,
        domain: &'a str,
        csr_der: &'a [u8],
        sink: &'a dyn ChallengeSink,
    ) -> BoxFut<'a, Result<String, AcmeError>> {
        Box::pin(async move {
            let slot = self.account.lock().await;
            let account = slot.as_ref().ok_or(AcmeError::TermsNeeded { url: None })?;
            let mut published = Vec::new();
            let result = self
                .run_order(account, domain, csr_der, sink, &mut published)
                .await;
            for token in &published {
                sink.withdraw(token);
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::fake::{FakeAcme, FakeCa};

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl ChallengeSink for Recorder {
        fn publish(&self, token: &str, key_authorization: &str) {
            self.0
                .lock()
                .unwrap()
                .push(format!("publish {token} {key_authorization}"));
        }
        fn withdraw(&self, token: &str) {
            self.0.lock().unwrap().push(format!("withdraw {token}"));
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("certs"));
        (dir, store)
    }

    #[tokio::test]
    async fn obtain_answers_the_challenge_and_stores_the_certificate() {
        let (_dir, store) = store();
        let ca = FakeAcme::new(FakeCa::new());
        let sink = Recorder::default();
        let stored = obtain(&ca, &store, "app.example.com", "staging", &sink, true)
            .await
            .unwrap();
        assert_eq!(stored.domain, "app.example.com");
        assert_eq!(stored.meta.server, "staging");
        assert_eq!(store.list().unwrap().certs.len(), 1);
        let log = sink.0.lock().unwrap().clone();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(
            log[0].starts_with("publish ") && log[1].starts_with("withdraw "),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn terms_must_be_accepted_once() {
        let (_dir, store) = store();
        let ca = FakeAcme::new(FakeCa::new()).with_terms("https://ca.example.org/terms");
        let sink = Recorder::default();
        let err = obtain(&ca, &store, "app.example.com", "production", &sink, false)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AcmeError::TermsNeeded { url: Some(u) } if u == "https://ca.example.org/terms"),
            "{err:?}"
        );
        assert_eq!(store.list().unwrap().certs, Vec::new());
        obtain(&ca, &store, "app.example.com", "production", &sink, true)
            .await
            .unwrap();
        // The account now exists: no acceptance needed again.
        obtain(&ca, &store, "b.example.com", "production", &sink, false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_failed_challenge_names_the_domain_and_keeps_the_old_certificate() {
        let (_dir, store) = store();
        let ca = FakeAcme::new(FakeCa::new());
        let sink = Recorder::default();
        let old = obtain(&ca, &store, "app.example.com", "production", &sink, true)
            .await
            .unwrap();
        ca.fail_with(
            "Invalid response from http://app.example.com/.well-known/acme-challenge/x: 404",
        );
        let err = obtain(&ca, &store, "app.example.com", "production", &sink, true)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("could not validate app.example.com") && msg.contains(": 404"),
            "{msg}"
        );
        assert_eq!(store.get("app.example.com").unwrap().unwrap(), old);
        // The token was withdrawn even though the order failed.
        assert!(
            sink.0
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .starts_with("withdraw ")
        );
    }

    #[tokio::test]
    async fn a_ca_that_never_finishes_times_out() {
        let (_dir, store) = store();
        let ca = FakeAcme::new(FakeCa::new()).hanging();
        let sink = Recorder::default();
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            obtain(&ca, &store, "app.example.com", "production", &sink, true),
        )
        .await;
        assert!(result.is_err(), "the caller's timeout bounds the order");
        assert_eq!(store.list().unwrap().certs, Vec::new());
    }

    #[test]
    fn server_ids() {
        assert_eq!(server_id(&AcmeServer::Production), "production");
        assert_eq!(server_id(&AcmeServer::Staging), "staging");
        let a = server_id(&AcmeServer::Url("https://localhost:14000/dir".into()));
        let b = server_id(&AcmeServer::Url("https://localhost:14001/dir".into()));
        assert!(a.starts_with("custom-") && a != b, "{a} {b}");
    }

    /// Answers HTTP-01 challenges itself on `127.0.0.1:<port>`.
    #[derive(Clone, Default)]
    struct ServedSink(Arc<Mutex<std::collections::HashMap<String, String>>>);

    impl ChallengeSink for ServedSink {
        fn publish(&self, token: &str, key_authorization: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(token.to_owned(), key_authorization.to_owned());
        }
        fn withdraw(&self, token: &str) {
            self.0.lock().unwrap().remove(token);
        }
    }

    impl ServedSink {
        async fn serve(&self, port: u16) {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .unwrap();
            let tokens = self.0.clone();
            tokio::spawn(async move {
                while let Ok((mut s, _)) = listener.accept().await {
                    let tokens = tokens.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 4096];
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let path = req.split_whitespace().nth(1).unwrap_or("");
                        let token = path.rsplit('/').next().unwrap_or("");
                        let body = tokens.lock().unwrap().get(token).cloned();
                        let resp = match body {
                            Some(b) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                                b.len()
                            ),
                            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
                        };
                        let _ = s.write_all(resp.as_bytes()).await;
                    });
                }
            });
        }
    }

    /// Runs against Pebble when `PEBBLE_DIRECTORY` is set (the CI `acme` job
    /// does; see `.github/workflows/ci.yml`). `PROXLET_ACME_ROOTS` must trust
    /// Pebble's TLS certificate. This test answers the challenge itself on
    /// `PEBBLE_HTTP_PORT`, where Pebble validates it (it resolves the name
    /// to 127.0.0.1 through `pebble-challtestsrv`).
    #[tokio::test]
    async fn pebble_issues_a_certificate() {
        let Some(dir_url) = std::env::var_os("PEBBLE_DIRECTORY") else {
            eprintln!("PEBBLE_DIRECTORY not set; skipping");
            return;
        };
        let port: u16 = std::env::var("PEBBLE_HTTP_PORT")
            .expect("PEBBLE_HTTP_PORT")
            .parse()
            .unwrap();
        let sink = ServedSink::default();
        sink.serve(port).await;

        let server = AcmeServer::Url(dir_url.to_string_lossy().into_owned());
        let tmp = tempfile::tempdir().unwrap();
        let ca = AcmeCa::new(&server, tmp.path(), Http::from_env().unwrap());
        assert!(
            ca.terms_url().await.unwrap().is_some(),
            "Pebble publishes terms"
        );
        let store = Store::new(tmp.path().join("certs"));
        let err = obtain(&ca, &store, "pebble.example.org", "custom", &sink, false)
            .await
            .unwrap_err();
        assert!(matches!(err, AcmeError::TermsNeeded { .. }), "{err:?}");
        let stored = obtain(&ca, &store, "pebble.example.org", "custom", &sink, true)
            .await
            .unwrap();
        assert!(stored.leaf.covers("pebble.example.org"));
        assert!(
            tmp.path()
                .join(server_id(&server))
                .join("account.json")
                .exists()
        );
        // A second order reuses the stored account.
        let ca = AcmeCa::new(&server, tmp.path(), Http::from_env().unwrap());
        obtain(&ca, &store, "pebble.example.org", "custom", &sink, false)
            .await
            .unwrap();
        assert!(sink.0.lock().unwrap().is_empty(), "tokens withdrawn");
    }
}
