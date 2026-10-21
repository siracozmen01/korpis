// SPDX-License-Identifier: AGPL-3.0-or-later

//! The portals this replica serves (§3.6 of docs/07-networking.md). It holds each portal whose
//! certificate can be served now, with a TLS configuration of its own, and reads them again
//! whenever the database announces a change to a portal or its certificate, and every minute
//! besides. The listener chooses a connection's configuration by the name the client sent in
//! SNI; a connection offering only `acme-tls/1` for a portal's name is answered with a
//! certificate made for the order's TLS-ALPN-01 challenge, read from the database for that
//! handshake, so whichever replica the directory reaches answers it.

use crate::api::Api;
use korpis_app::ports::{Store as _, Wakeups};
use rcgen::{CertificateParams, CustomExtension, KeyPair};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tokio::sync::watch;
use uuid::Uuid;

/// The protocol a TLS-ALPN-01 validation offers alone (RFC 8737 §6.2).
pub const ACME_TLS: &[u8] = b"acme-tls/1";

/// A portal, as a request it serves is told of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalAt {
    /// The portal.
    pub id: Uuid,
    /// Its organization.
    pub organization: Uuid,
    /// Its name, the host its requests must name.
    pub name: String,
    /// Its origin, the only one its cross-origin calls are answered for.
    pub origin: String,
}

/// A portal, served.
#[derive(Debug)]
pub struct Serving {
    /// What a request is told.
    pub at: PortalAt,
    /// The configuration its connections are served with.
    pub config: Arc<rustls::ServerConfig>,
    revision: i64,
}

/// The portals of the installation, as this replica serves them.
#[derive(Debug)]
pub struct Portals {
    api: Arc<Api>,
    port: Option<u16>,
    held: RwLock<Arc<Held>>,
    origins: watch::Sender<Arc<[String]>>,
}

/// What a replica holds of the portals: those it serves, by name, and the name of every portal.
#[derive(Debug, Default)]
struct Held {
    served: BTreeMap<String, Arc<Serving>>,
    names: std::collections::BTreeSet<String>,
}

/// What a connection naming a name is served with.
#[derive(Debug)]
pub enum Named {
    /// The portal it names.
    Portal(Arc<Serving>),
    /// A portal whose certificate cannot be served now: no handshake.
    Unserved,
    /// No portal: the installation's own certificate.
    Other,
}

/// A TLS configuration speaking HTTP/2 and HTTP/1.1 with the chain and key, both in PEM.
fn configured(chain: &str, key: &str) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = CertificateDer::pem_slice_iter(chain.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).map_err(|e| e.to_string())?;
    crate::tls::serving(certs, key).map_err(|e| e.to_string())
}

/// A TLS configuration answering a TLS-ALPN-01 validation of `name` with `digest`: a certificate
/// of its own, for the name alone, carrying the digest in its `acmeIdentifier` extension, and
/// `acme-tls/1` the one protocol spoken (RFC 8737 §3).
pub fn challenge_config(name: &str, digest: &[u8]) -> Result<Arc<rustls::ServerConfig>, String> {
    let mut params = CertificateParams::new(vec![name.to_owned()]).map_err(|e| e.to_string())?;
    params.custom_extensions = vec![CustomExtension::new_acme_identifier(digest)];
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    let key = PrivateKeyDer::Pkcs8(key.serialize_der().into());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let signing = provider.key_provider.load_private_key(key).map_err(|e| e.to_string())?;
    // Handed over as it is: the challenge's extension is critical, as RFC 8737 §3 says it must
    // be, and rustls refuses a certificate whose critical extension it does not know when it
    // checks one, which only the validating directory need read.
    let certified = rustls::sign::CertifiedKey::new(vec![cert.der().clone()], signing);
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(certified)));
    config.alpn_protocols = vec![ACME_TLS.to_vec()];
    Ok(Arc::new(config))
}

impl Portals {
    /// None yet, for `api`'s installation, reached on `port` (none for HTTPS's own).
    #[must_use]
    pub fn new(api: Arc<Api>, port: Option<u16>) -> Self {
        Self {
            api,
            port,
            held: RwLock::default(),
            origins: watch::Sender::new(Arc::from(Vec::new())),
        }
    }

    /// What a connection naming `name` is served with.
    #[must_use]
    pub fn named(&self, name: &str) -> Named {
        let Ok(held) = self.held.read() else { return Named::Other };
        match held.served.get(name) {
            Some(s) => Named::Portal(Arc::clone(s)),
            None if held.names.contains(name) => Named::Unserved,
            None => Named::Other,
        }
    }

    /// The origins of every portal served, and every change after: what a relay is told
    /// (§3.6 of docs/07-networking.md).
    #[must_use]
    pub fn origins(&self) -> watch::Receiver<Arc<[String]>> {
        self.origins.subscribe()
    }

    /// The configuration answering an order's TLS-ALPN-01 validation of `name`, if one is
    /// recorded for it now.
    pub async fn challenge(&self, name: &str) -> Option<Arc<rustls::ServerConfig>> {
        let digest = self.api.app().portal_challenge(name).await.ok()??;
        challenge_config(name, &digest)
            .inspect_err(|e| tracing::warn!(%name, error = %e, "a challenge could not be answered"))
            .ok()
    }

    /// Reads the portals served again, keeping the configuration of each whose certificate has
    /// not changed.
    async fn refresh(&self) -> Result<(), korpis_app::AppError> {
        let before = self.held.read().map(|h| Arc::clone(&h)).unwrap_or_default();
        let names = self.api.app().portal_names().await?.into_iter().collect();
        let mut next = BTreeMap::new();
        for s in self.api.app().portals_served().await? {
            let name = s.portal.name.as_str().to_owned();
            if let Some(kept) = before
                .served
                .get(&name)
                .filter(|k| k.revision == s.revision && k.at.id == s.portal.id)
            {
                next.insert(name, Arc::clone(kept));
                continue;
            }
            let config = match configured(&s.chain, &s.key) {
                Ok(config) => config,
                Err(e) => {
                    tracing::warn!(portal = %name, error = %e, "a portal's certificate is not served");
                    continue;
                }
            };
            let at = PortalAt {
                id: s.portal.id,
                organization: s.portal.organization,
                origin: s.portal.origin(self.port),
                name: name.clone(),
            };
            next.insert(name, Arc::new(Serving { at, config, revision: s.revision }));
        }
        let origins: Vec<String> = next.values().map(|s| s.at.origin.clone()).collect();
        if let Ok(mut held) = self.held.write() {
            *held = Arc::new(Held { served: next, names });
        }
        self.origins.send_if_modified(|o| {
            let changed = o.as_ref() != origins.as_slice();
            if changed {
                *o = Arc::from(origins);
            }
            changed
        });
        Ok(())
    }

    /// Serves the portals from now on, reading them again at each announcement and every minute.
    pub fn keep(self: &Arc<Self>) {
        let wakeups = self.api.app().store().portal_changes();
        tokio::spawn(follow(Arc::clone(self), wakeups));
    }
}

/// Reads `portals` again at each of `wakeups`, until they end.
async fn follow(portals: Arc<Portals>, mut wakeups: impl Wakeups) {
    loop {
        if let Err(e) = portals.refresh().await {
            tracing::warn!(error = %e, "the portals could not be read");
        }
        if wakeups.next().await.is_none() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_challenge_s_certificate_names_its_name_and_speaks_acme_tls_alone() {
        let config = challenge_config("panel.reseller.example", &[7; 32]).unwrap();
        assert_eq!(config.alpn_protocols, [ACME_TLS.to_vec()]);
    }
}
