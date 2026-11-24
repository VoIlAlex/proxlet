//! Serving TLS (design decision 5): a certificate resolver keyed by SNI name,
//! swapped atomically so new handshakes see new certificates while open
//! connections keep theirs.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use rustls::crypto::ring;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

pub use rustls::ServerConfig;

/// Certificates by lowercase DNS name.
pub type CertMap = HashMap<String, Arc<CertifiedKey>>;

/// Picks a certificate by the client's SNI name. A missing or unknown name
/// gets none, so the handshake fails.
#[derive(Default)]
pub struct CertResolver {
    certs: ArcSwap<CertMap>,
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertResolver")
            .field("names", &self.names())
            .finish()
    }
}

impl CertResolver {
    /// An empty resolver.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replaces every certificate at once. Handshakes already past certificate
    /// selection are unaffected.
    pub fn replace(&self, certs: CertMap) {
        self.certs.store(Arc::new(certs));
    }

    /// The names that currently have a certificate, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.certs.load().keys().cloned().collect();
        names.sort();
        names
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = hello.server_name()?.to_ascii_lowercase();
        self.certs.load().get(&name).cloned()
    }
}

/// Builds a rustls key from a PEM file holding the chain and the private key.
///
/// # Errors
///
/// When the PEM has no certificate, no key, or a key rustls cannot use.
pub fn certified_key(pem: &[u8]) -> Result<CertifiedKey, String> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("unreadable certificate: {e}"))?;
    if chain.is_empty() {
        return Err("no certificate in the file".into());
    }
    let key =
        PrivateKeyDer::from_pem_slice(pem).map_err(|e| format!("unreadable private key: {e}"))?;
    let signing = ring::sign::any_supported_type(&key)
        .map_err(|e| format!("unsupported private key: {e}"))?;
    Ok(CertifiedKey::new(chain, signing))
}

/// The server configuration for the HTTPS listener: TLS 1.2 and 1.3, ALPN
/// `h2` and `http/1.1`, certificates from `resolver`.
///
/// # Panics
///
/// If the ring provider rejects its own default protocol versions.
#[must_use]
pub fn server_config(resolver: Arc<CertResolver>) -> Arc<rustls::ServerConfig> {
    let mut config =
        rustls::ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_no_client_auth()
            .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}
