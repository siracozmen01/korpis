// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay's certificate, kept current (§7 of docs/18-operations.md). The relay renews it with a
//! request its identity key signs over the new TLS key, and is issued a certificate as at its
//! join; the control plane reports each step of an unrenewed certificate's approach to expiry as
//! an event, once, so an operator hears of a relay that stopped renewing well before the
//! certificate runs out.

use super::join::lifetime;
use crate::App;
use crate::error::AppError;
use crate::node::NodeProof;
use crate::ports::{Clock, EffectWrite as _, IssuedCertificate, RecipeSource, RelayWrite as _};
use crate::ports::{Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::node::identity::relay_renew_message;
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::relay::{CertificateAlert, certificate_alert};
use uuid::Uuid;

/// Whether `key` is an uncompressed P-256 point, as a relay's TLS key must be.
pub(super) fn valid_tls_key(key: &[u8]) -> Result<(), AppError> {
    if key.len() == 65 && key.first() == Some(&4) {
        Ok(())
    } else {
        Err(AppError::invalid("tls_key", "an uncompressed P-256 point, 65 bytes"))
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A fresh certificate for `relay`'s new `tls_key`, if `proof` is the relay's fresh signature
    /// over a renewal naming that key and the relay is not removed.
    pub async fn renew_relay(
        &self,
        relay: Uuid,
        proof: &NodeProof,
        tls_key: &[u8],
    ) -> Result<IssuedCertificate, AppError> {
        valid_tls_key(tls_key)?;
        let issuer = self
            .certificate_issuer
            .clone()
            .ok_or_else(|| AppError::Internal("no certificate authority is configured".into()))?;
        let message =
            |r: &str, s, ns, nonce: &[u8]| relay_renew_message((r, tls_key), s, ns, nonce);
        let found = self.check_relay(relay, proof, &message).await?;
        let certificate = issuer
            .issue(found.url.host(), tls_key, lifetime(found.id))
            .map_err(AppError::Internal)?;
        let effect = Effect::new(
            Principal::System(SystemActor::Authority),
            "relay.certificate_renew",
            Outcome::Succeeded,
        )
        .after(details([
            ("relay", relay.to_string().into()),
            ("name", found.name.as_str().into()),
            ("expires_at_unix", unix_seconds(certificate.expires_at).into()),
        ]));
        let mut tx = self.store.begin().await?;
        tx.record_relay_certificate(relay, certificate.expires_at).await?;
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(certificate)
    }

    /// Reports, as an event, each relay certificate that came a step closer to expiring since it
    /// was last reported; how many.
    pub async fn alert_relay_certificates(&self) -> Result<usize, AppError> {
        let now = self.clock.now();
        let mut reported = 0;
        for c in self.store.relay_certificates().await? {
            let Some(severity) = certificate_alert(c.expires_at, now) else { continue };
            if c.alerted >= Some(severity) {
                continue;
            }
            let alert = CertificateAlert {
                relay: c.relay.id,
                name: c.relay.name.as_str().to_owned(),
                organization: c.organization,
                expires_at: c.expires_at,
                severity,
                at: now,
            };
            let mut tx = self.store.begin().await?;
            if tx.alert_relay_certificate(&alert, c.alerted).await? {
                tx.commit().await?;
                reported += 1;
            }
        }
        Ok(reported)
    }
}

/// `at` in whole seconds since the Unix epoch.
fn unix_seconds(at: std::time::SystemTime) -> i64 {
    let since = at.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
}
