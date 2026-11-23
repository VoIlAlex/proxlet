//! A local certificate authority for tests: signs leaf certificates for any
//! name with any validity period, without a network.

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use time::OffsetDateTime;

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
