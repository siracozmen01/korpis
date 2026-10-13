// SPDX-License-Identifier: AGPL-3.0-or-later

//! The certificate a relay serves with, replaceable while it serves: a renewal swaps it in, and
//! every handshake after uses the new one while connections already open keep theirs.

use super::dir::TlsFiles;
use crate::tls::TlsError;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::sync::{Arc, PoisonError, RwLock};

/// The certificate served now.
#[derive(Debug)]
pub struct Served {
    current: RwLock<Arc<CertifiedKey>>,
}

/// The key and chain of `files`, checked to belong together.
pub fn load(files: &TlsFiles) -> Result<CertifiedKey, TlsError> {
    let pem = |e: rustls::pki_types::pem::Error| TlsError::Pem(e.to_string());
    let chain = CertificateDer::pem_file_iter(&files.chain)
        .map_err(pem)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(pem)?;
    let key = PrivateKeyDer::from_pem_file(&files.key).map_err(pem)?;
    let provider = rustls::crypto::ring::default_provider();
    Ok(CertifiedKey::from_der(chain, key, &provider)?)
}

impl Served {
    /// Serving `first`.
    #[must_use]
    pub fn new(first: CertifiedKey) -> Self {
        Self { current: RwLock::new(Arc::new(first)) }
    }

    /// Serves `next` from the next handshake on.
    pub fn replace(&self, next: CertifiedKey) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(next);
    }

    /// The chain served now, leaf first.
    #[must_use]
    pub fn chain(&self) -> Vec<CertificateDer<'static>> {
        self.current.read().unwrap_or_else(PoisonError::into_inner).cert.clone()
    }
}

impl ResolvesServerCert for Served {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner)))
    }
}
