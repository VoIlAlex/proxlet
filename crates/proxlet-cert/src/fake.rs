//! A local certificate authority for tests: signs leaf certificates for any
//! name with any validity period, without a network.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::CertificateSigningRequestDer;
use time::OffsetDateTime;

use crate::acme::{AcmeError, BoxFut, Ca, ChallengeSink};

/// A fake CA with a self-signed root.
pub struct FakeCa {
    issuer: Issuer<'static, KeyPair>,
    root_pem: String,
}

impl std::fmt::Debug for FakeCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeCa").finish_non_exhaustive()
    }
}

impl Default for FakeCa {
    fn default() -> Self {
        Self::new()
    }
}

fn at(unix: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(unix).expect("timestamp in range")
}

impl FakeCa {
    /// A new CA with a fresh P-256 key.
    ///
    /// # Panics
    ///
    /// If key generation fails.
    #[must_use]
    pub fn new() -> Self {
        let key = KeyPair::generate().expect("generate CA key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "proxlet fake CA");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root_pem = params.self_signed(&key).expect("self-sign CA").pem();
        Self {
            issuer: Issuer::new(params, key),
            root_pem,
        }
    }

    /// The root certificate, for clients that should trust this CA.
    #[must_use]
    pub fn root_pem(&self) -> &str {
        &self.root_pem
    }

    /// Signs a leaf for `domain` with `key`, valid from `not_before` to
    /// `not_after` (Unix seconds). Returns the chain PEM (leaf, then root).
    ///
    /// # Panics
    ///
    /// If signing fails.
    #[must_use]
    pub fn sign(&self, domain: &str, key: &KeyPair, not_before: i64, not_after: i64) -> String {
        let mut params = CertificateParams::new(vec![domain.to_owned()]).expect("leaf params");
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, domain);
        params.distinguished_name = dn;
        params.not_before = at(not_before);
        params.not_after = at(not_after);
        let leaf = params.signed_by(key, &self.issuer).expect("sign leaf");
        format!("{}{}", leaf.pem(), self.root_pem)
    }

    /// Signs the CSR (as an ACME server would on finalize), valid from
    /// `not_before` to `not_after`. Returns the chain PEM.
    ///
    /// # Errors
    ///
    /// When the CSR cannot be parsed or signed.
    pub fn sign_csr(
        &self,
        csr_der: &[u8],
        not_before: i64,
        not_after: i64,
    ) -> Result<String, String> {
        let der = CertificateSigningRequestDer::from(csr_der.to_vec());
        let mut csr = CertificateSigningRequestParams::from_der(&der).map_err(|e| e.to_string())?;
        csr.params.not_before = at(not_before);
        csr.params.not_after = at(not_after);
        let leaf = csr.signed_by(&self.issuer).map_err(|e| e.to_string())?;
        Ok(format!("{}{}", leaf.pem(), self.root_pem))
    }

    /// A fresh key and a chain for `domain`, valid from now for `valid_secs`.
    /// Returns `(chain_pem, key_pem)`.
    ///
    /// # Panics
    ///
    /// If key generation or signing fails.
    #[must_use]
    pub fn issue(&self, domain: &str, valid_secs: i64) -> (String, String) {
        let now = crate::now();
        self.issue_between(domain, now, now + valid_secs)
    }

    /// Like [`Self::issue`] with explicit dates.
    ///
    /// # Panics
    ///
    /// If key generation or signing fails.
    #[must_use]
    pub fn issue_between(&self, domain: &str, not_before: i64, not_after: i64) -> (String, String) {
        let key = KeyPair::generate().expect("generate leaf key");
        let chain = self.sign(domain, &key, not_before, not_after);
        (chain, key.serialize_pem())
    }
}

/// A [`Ca`] backed by a [`FakeCa`]: publishes a token to the sink, then signs
/// the CSR. It can be told to demand the terms, fail, or never finish.
pub struct FakeAcme {
    ca: FakeCa,
    terms: Option<String>,
    account: AtomicBool,
    valid_secs: AtomicI64,
    fail: Mutex<Option<String>>,
    hang: bool,
    issued: AtomicUsize,
}

impl std::fmt::Debug for FakeAcme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeAcme")
            .field("issued", &self.issued.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl FakeAcme {
    /// A CA that issues 90-day certificates and has no terms.
    #[must_use]
    pub fn new(ca: FakeCa) -> Self {
        Self {
            ca,
            terms: None,
            account: AtomicBool::new(false),
            valid_secs: AtomicI64::new(90 * 86_400),
            fail: Mutex::new(None),
            hang: false,
            issued: AtomicUsize::new(0),
        }
    }

    /// Publishes terms at `url` (still accepted by `accept_terms`).
    #[must_use]
    pub fn with_terms(mut self, url: &str) -> Self {
        self.terms = Some(url.to_owned());
        self
    }

    /// Never finishes an order.
    #[must_use]
    pub fn hanging(mut self) -> Self {
        self.hang = true;
        self
    }

    /// Fails every order from now on with the CA message `why`.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    pub fn fail_with(&self, why: &str) {
        *self.fail.lock().unwrap() = Some(why.to_owned());
    }

    /// Succeeds again after [`Self::fail_with`].
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    pub fn succeed(&self) {
        *self.fail.lock().unwrap() = None;
    }

    /// Issues certificates valid for `secs` from now.
    pub fn set_validity(&self, secs: i64) {
        self.valid_secs.store(secs, Ordering::SeqCst);
    }

    /// How many certificates were issued.
    #[must_use]
    pub fn issued(&self) -> usize {
        self.issued.load(Ordering::SeqCst)
    }

    /// The CA behind it.
    #[must_use]
    pub fn ca(&self) -> &FakeCa {
        &self.ca
    }
}

impl Ca for FakeAcme {
    fn ensure_account(&self, accept_terms: bool) -> BoxFut<'_, Result<(), AcmeError>> {
        Box::pin(async move {
            if !self.account.load(Ordering::SeqCst) {
                if !accept_terms {
                    return Err(AcmeError::TermsNeeded {
                        url: self.terms.clone(),
                    });
                }
                self.account.store(true, Ordering::SeqCst);
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
            if self.hang {
                std::future::pending::<()>().await;
            }
            let n = self.issued.load(Ordering::SeqCst);
            let token = format!("fake-token-{n}-{domain}");
            sink.publish(&token, &format!("{token}.fake-thumbprint"));
            let failure = self.fail.lock().unwrap().clone();
            let result = if let Some(why) = failure {
                Err(AcmeError::Refused(format!(
                    "the ACME server could not validate {domain}: {why}"
                )))
            } else {
                let now = crate::now();
                let until = now + self.valid_secs.load(Ordering::SeqCst);
                self.ca
                    .sign_csr(csr_der, now, until)
                    .map_err(AcmeError::Refused)
                    .inspect(|_| {
                        self.issued.fetch_add(1, Ordering::SeqCst);
                    })
            };
            sink.withdraw(&token);
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_dates_are_the_chosen_ones() {
        let ca = FakeCa::new();
        let (chain, key) = ca.issue_between("app.example.com", 1_800_000_000, 1_803_888_000);
        let leaf = crate::pem::validate("app.example.com", &chain, &key).unwrap();
        assert_eq!(
            (leaf.not_before, leaf.not_after),
            (1_800_000_000, 1_803_888_000)
        );
        assert_eq!(leaf.issuer, "proxlet fake CA");
        assert!(chain.ends_with(ca.root_pem()));
    }
}
