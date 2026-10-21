// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a replica's listener serves the portals with (§3.6 of docs/07-networking.md): each
//! portal whose certificate is issued and valid now, its chain and its key opened from the seal
//! the control plane's TLS key put on it, and, while an order runs, the TLS-ALPN-01 digest the
//! directory's connection for a portal's name is answered with.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::dns::portal::Portal;
use zeroize::Zeroizing;

/// A portal as a listener serves it.
pub struct Served {
    /// The portal.
    pub portal: Portal,
    /// Its certificate's chain, leaf first, in PEM.
    pub chain: String,
    /// Its certificate's key, in PEM.
    pub key: Zeroizing<String>,
    /// Its certificate's revision, so a listener knows when to read it again.
    pub revision: i64,
}

impl std::fmt::Debug for Served {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Served").field("portal", &self.portal).finish_non_exhaustive()
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Every portal whose certificate can be served now, with its chain and key. One whose
    /// certificate is not issued, or not valid now, is left out, and its name gets no handshake;
    /// its own record says why (§3.6 of docs/07-networking.md).
    pub async fn portals_served(&self) -> Result<Vec<Served>, AppError> {
        let now = self.clock.now();
        let tls = self.tls()?;
        let mut served = Vec::new();
        for portal in self.store.all_portals().await? {
            let Some(c) = self.store.certificate(portal.certificate).await? else { continue };
            if !c.valid_at(now) {
                continue;
            }
            let Some(sealed) = self.store.sealed_key(c.id).await? else { continue };
            let key = tls.open(c.id, &sealed).map_err(|e| AppError::Internal(e.to_string()))?;
            served.push(Served { portal, chain: c.chain, key, revision: c.revision });
        }
        Ok(served)
    }

    /// The name of every portal, served or not: a name among them whose certificate cannot be
    /// served gets no handshake, rather than the installation's own certificate.
    pub async fn portal_names(&self) -> Result<Vec<String>, AppError> {
        let all = self.store.all_portals().await?;
        Ok(all.into_iter().map(|p| p.name.as_str().to_owned()).collect())
    }

    /// The TLS-ALPN-01 digest an order for the portal name `name` is answering with now.
    pub async fn portal_challenge(&self, name: &str) -> Result<Option<Vec<u8>>, AppError> {
        Ok(self.store.portal_challenge(name).await?)
    }
}
