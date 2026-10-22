// SPDX-License-Identifier: AGPL-3.0-or-later

//! TLS for the routes the proxy terminates (§3.2 of docs/07-networking.md): TLS 1.2 and 1.3,
//! HTTP/2 and HTTP/1.1 offered by ALPN, and the certificate chosen by the name the client asks
//! for, from the table as it is when the client asks. One configuration serves every connection
//! to a route that asks for no client certificate, so a client resumes a session with any of
//! them. A route that asks for one has a configuration of its own, made with its table, whose
//! sessions and tickets are its own: a session made where no certificate was asked for is never
//! resumed where one is, and the authorities that verify a client's are that route's alone.

use crate::table::Table;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use rustls::server::danger::ClientCertVerifier;
use rustls::server::{ClientHello, ResolvesServerCert, WebPkiClientVerifier};
use rustls::sign::CertifiedKey;
use std::sync::Arc;
use tokio::sync::watch;

/// Chooses the certificate of the route the client's SNI names.
#[derive(Debug)]
struct BySni(watch::Receiver<Arc<Table>>);

impl ResolvesServerCert for BySni {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let table = Arc::clone(&self.0.borrow());
        table.route(hello.server_name()?)?.certificate.clone()
    }
}

/// The configuration every terminated connection is accepted with.
pub fn server_config(
    table: watch::Receiver<Arc<Table>>,
) -> Result<Arc<rustls::ServerConfig>, rustls::Error> {
    let mut config = rustls::ServerConfig::builder_with_provider(crate::unverified::provider())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(BySni(table)));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config.ticketer = rustls::crypto::ring::Ticketer::new()?;
    Ok(Arc::new(config))
}

/// Serves one route's certificate, or none while it has none.
#[derive(Debug)]
struct Fixed(Option<Arc<CertifiedKey>>);

impl ResolvesServerCert for Fixed {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.0.clone()
    }
}

/// The configuration a route that asks for client certificates is accepted with: its
/// `certificate`, and `clients` verifying what a client presents.
pub fn asking_clients(
    certificate: Option<Arc<CertifiedKey>>,
    clients: Arc<dyn ClientCertVerifier>,
) -> Result<Arc<rustls::ServerConfig>, rustls::Error> {
    let mut config = rustls::ServerConfig::builder_with_provider(crate::unverified::provider())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
        .with_client_cert_verifier(clients)
        .with_cert_resolver(Arc::new(Fixed(certificate)));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config.ticketer = rustls::crypto::ring::Ticketer::new()?;
    Ok(Arc::new(config))
}

/// What verifies a client's certificate against the authorities `pem` holds, a client without
/// one let through unless `required`. No revocation is checked: no CRL is given, and none is
/// fetched (§6 of docs/17-security.md).
pub fn clients(pem: &str, required: bool) -> Result<Arc<dyn ClientCertVerifier>, String> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        roots.add(cert.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    if roots.is_empty() {
        return Err("no authority to verify a client's certificate against".into());
    }
    let builder =
        WebPkiClientVerifier::builder_with_provider(Arc::new(roots), crate::unverified::provider());
    let builder = if required { builder } else { builder.allow_unauthenticated() };
    builder.build().map_err(|e| e.to_string())
}

/// The configuration a TLS-ALPN-01 validation of `name` is answered with (RFC 8737 §3): a
/// certificate of its own, for the name alone, carrying `digest` in its critical
/// `acmeIdentifier` extension, and `acme-tls/1` the one protocol spoken. Made for each
/// validation, as the control plane makes a portal's, since a directory validates a name a few
/// times at most.
pub fn alpn_challenge(name: &str, digest: &[u8]) -> Result<Arc<rustls::ServerConfig>, String> {
    let mut params =
        rcgen::CertificateParams::new(vec![name.to_owned()]).map_err(|e| e.to_string())?;
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
    let provider = crate::unverified::provider();
    let signing = provider.key_provider.load_private_key(key).map_err(|e| e.to_string())?;
    let certified = CertifiedKey::new(vec![cert.der().clone()], signing);
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(Some(Arc::new(certified)))));
    config.alpn_protocols = vec![crate::sni::ACME_TLS.to_vec()];
    Ok(Arc::new(config))
}
