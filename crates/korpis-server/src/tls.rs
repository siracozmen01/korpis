// SPDX-License-Identifier: AGPL-3.0-or-later

//! TLS. An installation is its own certificate authority: `install` creates a CA and a server
//! certificate for the names the operator gives, and clients and agents pin the CA (§3 of
//! docs/18-operations.md). An operator may replace the server certificate with one from a public
//! CA; the files are ordinary PEM.

use crate::data::{DataDir, write_public, write_secret};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Arc;

/// Why TLS material could not be created or loaded.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// Certificate generation failed.
    #[error("generating certificates: {0}")]
    Generate(#[from] rcgen::Error),
    /// Reading or writing a file failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// A PEM file is malformed.
    #[error("reading PEM: {0}")]
    Pem(String),
    /// The configuration was refused.
    #[error("TLS configuration: {0}")]
    Config(#[from] rustls::Error),
}

/// The installation authority's parameters. Issuing under it later rebuilds the issuer from
/// these and the authority's key, so they are written once, here: a certificate issued under
/// different parameters would name an issuer that does not exist.
pub(crate) fn ca_params() -> Result<CertificateParams, TlsError> {
    let mut ca = CertificateParams::new(Vec::<String>::new())?;
    ca.distinguished_name.push(DnType::CommonName, "Korpis installation CA");
    ca.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    Ok(ca)
}

/// Creates the CA and a server certificate valid for `names` (DNS names or IP addresses).
pub fn generate(data: &DataDir, names: &[String]) -> Result<(), TlsError> {
    let ca_key = KeyPair::generate()?;
    let ca = ca_params()?;
    let ca_cert = ca.self_signed(&ca_key)?;

    let server_key = KeyPair::generate()?;
    let mut server = CertificateParams::new(names.to_vec())?;
    server.distinguished_name.push(DnType::CommonName, "korpis-server");
    let issuer = rcgen::Issuer::from_params(&ca, &ca_key);
    let server_cert = server.signed_by(&server_key, &issuer)?;

    let dir = data.tls();
    write_public(&dir.join("ca.pem"), ca_cert.pem().as_bytes())?;
    write_secret(&dir.join("ca.key"), ca_key.serialize_pem().as_bytes())?;
    write_public(&dir.join("server.pem"), server_cert.pem().as_bytes())?;
    write_secret(&dir.join("server.key"), server_key.serialize_pem().as_bytes())?;
    Ok(())
}

/// `sha256:` of the DER of the installation's CA: what an enrollment token's join command
/// carries, so an agent verifies its first connection rather than trusting it (§3 of
/// docs/18-operations.md).
pub fn ca_fingerprint(data: &DataDir) -> Result<String, TlsError> {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let ca = CertificateDer::from_pem_file(data.tls().join("ca.pem"))
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    let digest = Sha256::digest(ca.as_ref());
    Ok(digest.iter().fold(String::from("sha256:"), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// The server configuration, speaking HTTP/2 and HTTP/1.1.
pub fn server_config(data: &DataDir) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let dir = data.tls();
    let mut certs = CertificateDer::pem_file_iter(dir.join("server.pem"))
        .map_err(|e| TlsError::Pem(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    // The certificate install issued is presented with the installation CA behind it, so an
    // enrolling agent can pin the CA by the fingerprint its join command carries (§3 of
    // docs/18-operations.md). A server.pem an operator replaced carries its own chain.
    if certs.len() == 1 {
        let ca = CertificateDer::from_pem_file(dir.join("ca.pem"))
            .map_err(|e| TlsError::Pem(e.to_string()))?;
        certs.push(ca);
    }
    let key = PrivateKeyDer::from_pem_file(dir.join("server.key"))
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    serving(certs, key)
}

/// A server configuration presenting whatever certificate `resolver` holds at each handshake,
/// speaking HTTP/2 and HTTP/1.1: what a relay running as its own process serves with, so a
/// renewed certificate is served without a restart.
pub fn resolving(
    resolver: Arc<dyn rustls::server::ResolvesServerCert>,
) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Speaking HTTP/2 and HTTP/1.1 with `certs` and `key`.
pub(crate) fn serving(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
