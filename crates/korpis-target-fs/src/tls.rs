// SPDX-License-Identifier: AGPL-3.0-or-later

//! The store's own certificate, made when the provider starts and never written anywhere: every
//! grant carries it, and a reader trusts it and no other (§5.6 of docs/06-storage.md). A provider
//! that restarts makes another, and the grants it made before are gone with their tokens.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;

/// The certificate and the TLS that presents it.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The certificate, DER.
    pub certificate: Vec<u8>,
    /// TLS presenting it.
    pub config: Arc<rustls::ServerConfig>,
}

/// Why no identity could be made.
#[derive(Debug, thiserror::Error)]
#[error("the store's certificate: {0}")]
pub struct IdentityError(String);

/// A new identity: an ECDSA P-256 key and a certificate for it. Its name is never checked, since
/// readers match the certificate whole.
///
/// # Errors
/// The key or certificate cannot be made.
pub fn identity() -> Result<Identity, IdentityError> {
    let fail = |e: &dyn std::fmt::Display| IdentityError(e.to_string());
    let key = rcgen::KeyPair::generate().map_err(|e| fail(&e))?;
    let params =
        rcgen::CertificateParams::new(vec!["korpis-target-fs".to_owned()]).map_err(|e| fail(&e))?;
    let cert = params.self_signed(&key).map_err(|e| fail(&e))?;
    let der = cert.der().to_vec();
    let private = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| fail(&e))?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(der.clone())], private)
        .map_err(|e| fail(&e))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Identity { certificate: der, config: Arc::new(config) })
}
