// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whom a node or a relay trusts (§3 of docs/18-operations.md, §7 of docs/02-architecture.md).
//! When it joins it knows only the fingerprint the join command carries: the control plane
//! presents the installation CA behind its certificate, and the connection is accepted only if
//! that CA has the fingerprint and the certificate verifies under it. The CA is kept, and from
//! then on trusted alone, so the first connection is verified rather than trusted, and every
//! later one is pinned. The agent and a relay running as its own process share this, so the two
//! cannot disagree about what joining trusts.

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

/// Why a TLS configuration could not be built.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The stored CA does not parse.
    #[error("the pinned CA: {0}")]
    Ca(String),
    /// rustls refused the configuration.
    #[error("TLS: {0}")]
    Config(#[from] rustls::Error),
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// `sha256:` and the hexadecimal digest of a certificate's DER.
#[must_use]
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().fold(String::from("sha256:"), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Verifies a server under the CA in its chain with a given fingerprint, and remembers that CA.
#[derive(Debug)]
pub struct Pinned {
    fingerprint: String,
    found: Mutex<Option<Vec<u8>>>,
}

impl Pinned {
    /// The CA it verified the server under, PEM, once a handshake has.
    #[must_use]
    pub fn ca_pem(&self) -> Option<Vec<u8>> {
        let der = self.found.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
        let body = base64_lines(&der);
        Some(format!("-----BEGIN CERTIFICATE-----\n{body}-----END CERTIFICATE-----\n").into_bytes())
    }
}

fn base64_lines(der: &[u8]) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 64 + 1);
    for line in encoded.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let ca = intermediates
            .iter()
            .find(|c| fingerprint(c.as_ref()) == self.fingerprint)
            .ok_or_else(|| rustls::Error::General("no CA with the pinned fingerprint".into()))?;
        let mut roots = RootCertStore::empty();
        roots.add(ca.clone())?;
        let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .map_err(|e| rustls::Error::General(e.to_string()))?;
        let verified =
            inner.verify_server_cert(end_entity, &[], server_name, ocsp_response, now)?;
        *self.found.lock().unwrap_or_else(PoisonError::into_inner) = Some(ca.as_ref().to_vec());
        Ok(verified)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = provider().signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, cert, dss, &algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = provider().signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, cert, dss, &algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

/// A configuration for enrollment, pinning the CA with `fingerprint`; and the verifier, which
/// holds the CA once a handshake has succeeded.
pub fn pinned(fingerprint: &str) -> Result<(rustls::ClientConfig, Arc<Pinned>), TlsError> {
    let verifier =
        Arc::new(Pinned { fingerprint: fingerprint.to_owned(), found: Mutex::new(None) });
    let dynamic: Arc<dyn ServerCertVerifier> = verifier.clone();
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(dynamic)
        .with_no_client_auth();
    Ok((config, verifier))
}

/// A configuration trusting the stored CA alone.
pub fn trusting(ca_pem: &[u8]) -> Result<rustls::ClientConfig, TlsError> {
    let ca = CertificateDer::from_pem_slice(ca_pem).map_err(|e| TlsError::Ca(e.to_string()))?;
    let mut roots = RootCertStore::empty();
    roots.add(ca)?;
    Ok(rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}
