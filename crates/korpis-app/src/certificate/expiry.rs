// SPDX-License-Identifier: AGPL-3.0-or-later

//! Telling a certificate's organization that it nears expiry (§3.2 of docs/07-networking.md):
//! a `certificate.expiring` event, a warning and then critical (`expiry_alert`), each reported
//! once for one validity period. A `provided` certificate is renewed only by its organization, so
//! this is how it learns to; an `acme` one is told only when its renewal is late.

use crate::App;
use crate::error::AppError;
use crate::ports::{CertificateWrite as _, Clock, RecipeSource, Store, Tx as _};
use korpis_core::dns::certificate::{ExpiryAlert, expiry_alert};

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Reports each certificate that came a step closer to expiring; how many were reported.
    pub async fn alert_certificates(&self) -> Result<usize, AppError> {
        let now = self.clock.now();
        let mut reported = 0;
        for c in self.store.certificate_expiries().await? {
            let Some(severity) = expiry_alert(c.not_before, c.not_after, now) else { continue };
            if c.alerted >= Some(severity) {
                continue;
            }
            let alert = ExpiryAlert {
                certificate: c.certificate,
                organization: c.organization,
                names: c.names,
                source: c.source,
                not_after: c.not_after,
                severity,
                at: now,
            };
            let mut tx = self.store.begin().await?;
            if tx.alert_certificate(&alert, c.alerted).await? {
                tx.commit().await?;
                reported += 1;
            }
        }
        Ok(reported)
    }
}
