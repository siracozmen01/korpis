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

/// Creates the CA and a server certificate valid for `names` (DNS names or IP addresses).
pub fn generate(data: &DataDir, names: &[String]) -> Result<(), TlsError> {
    let ca_key = KeyPair::generate()?;
    let mut ca = CertificateParams::new(Vec::<String>::new())?;
    ca.distinguished_name.push(DnType::CommonName, "Korpis installation CA");
    ca.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
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

/// The server configuration, speaking HTTP/2 and HTTP/1.1.
pub fn server_config(data: &DataDir) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let dir = data.tls();
    let certs = CertificateDer::pem_file_iter(dir.join("server.pem"))
        .map_err(|e| TlsError::Pem(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    let key = PrivateKeyDer::from_pem_file(dir.join("server.key"))
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
