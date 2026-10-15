// SPDX-License-Identifier: AGPL-3.0-or-later

//! TLS for the routes the proxy terminates (§3.2 of docs/07-networking.md): TLS 1.2 and 1.3,
//! HTTP/2 and HTTP/1.1 offered by ALPN, and the certificate chosen by the name the client asks
//! for, from the table as it is when the client asks. One configuration serves every connection,
//! so a client resumes a session with any of them.

use crate::table::Table;
use rustls::server::{ClientHello, ResolvesServerCert};
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
