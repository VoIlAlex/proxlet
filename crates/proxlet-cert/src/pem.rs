//! Reading the facts proxlet needs from a certificate chain and key.

use rcgen::{KeyPair, PublicKeyData};
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

use crate::CertError;

/// What proxlet needs to know about a stored certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    /// The DNS names the certificate covers, lowercase.
    pub names: Vec<String>,
    /// Start of validity, Unix seconds.
    pub not_before: i64,
    /// End of validity, Unix seconds.
    pub not_after: i64,
    /// The issuer's common name, e.g. `R11`.
    pub issuer: String,
}

impl Leaf {
    /// Whether the certificate covers `domain`.
    #[must_use]
    pub fn covers(&self, domain: &str) -> bool {
        self.names.iter().any(|n| n == domain)
    }
}

/// The DER of every `CERTIFICATE` block in `pem`, in order.
fn certificates(pem: &str) -> Result<Vec<Vec<u8>>, CertError> {
    let mut out = Vec::new();
    for block in x509_parser::pem::Pem::iter_from_buffer(pem.as_bytes()) {
        let block = block.map_err(|e| CertError::Invalid(format!("unreadable PEM: {e}")))?;
        if block.label == "CERTIFICATE" {
            out.push(block.contents);
        }
    }
    Ok(out)
}

/// The text of the first private key block in `pem`, if any.
#[must_use]
pub fn private_key(pem: &str) -> Option<&str> {
    let start = pem
        .find("-----BEGIN PRIVATE KEY-----")
        .or_else(|| pem.find("-----BEGIN EC PRIVATE KEY-----"))
        .or_else(|| pem.find("-----BEGIN RSA PRIVATE KEY-----"))?;
    let rest = &pem[start..];
    let end_marker = rest.find("-----END ")?;
    let end = rest[end_marker..]
        .find("-----\n")
        .map_or(rest.len(), |i| end_marker + i + 6);
    Some(&rest[..end.min(rest.len())])
}

/// Parses the leaf (first) certificate of a chain.
///
/// # Errors
///
/// When there is no certificate or it cannot be parsed.
pub fn leaf(chain_pem: &str) -> Result<Leaf, CertError> {
    let certs = certificates(chain_pem)?;
    let der = certs
        .first()
        .ok_or_else(|| CertError::Invalid("no certificate in the chain".into()))?;
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| CertError::Invalid(format!("unreadable certificate: {e}")))?;
    Ok(leaf_facts(&cert))
}

fn leaf_facts(cert: &X509Certificate<'_>) -> Leaf {
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for n in &san.value.general_names {
            if let GeneralName::DNSName(d) = n {
                names.push(d.to_ascii_lowercase());
            }
        }
    }
    let issuer = cert
        .issuer()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .unwrap_or_default()
        .to_owned();
    let validity = cert.validity();
    Leaf {
        names,
        not_before: validity.not_before.timestamp(),
        not_after: validity.not_after.timestamp(),
        issuer,
    }
}

/// Checks that `key_pem` is the private key of the chain's leaf and that the
/// leaf covers `domain`, returning the leaf's facts.
///
/// # Errors
///
/// When the chain or key cannot be read, they do not match, or the leaf does
/// not name `domain`.
pub fn validate(domain: &str, chain_pem: &str, key_pem: &str) -> Result<Leaf, CertError> {
    let certs = certificates(chain_pem)?;
    let der = certs
        .first()
        .ok_or_else(|| CertError::Invalid("no certificate in the chain".into()))?;
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| CertError::Invalid(format!("unreadable certificate: {e}")))?;
    let key = KeyPair::from_pem(key_pem)
        .map_err(|e| CertError::Invalid(format!("unreadable private key: {e}")))?;
    if key.subject_public_key_info() != cert.public_key().raw {
        return Err(CertError::Invalid(
            "the private key does not belong to the certificate".into(),
        ));
    }
    let facts = leaf_facts(&cert);
    if !facts.covers(domain) {
        return Err(CertError::Invalid(format!(
            "the certificate is for {} rather than {domain}",
            if facts.names.is_empty() {
                "no DNS name".to_owned()
            } else {
                facts.names.join(", ")
            }
        )));
    }
    if facts.not_after <= facts.not_before {
        return Err(CertError::Invalid(
            "the certificate's dates are inverted".into(),
        ));
    }
    Ok(facts)
}
