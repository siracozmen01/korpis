// SPDX-License-Identifier: AGPL-3.0-or-later

//! Revoking what a node removed as compromised was given (§9 of docs/17-security.md): every chain
//! of an `acme` certificate it was given, each through the directory with `keyCompromise`. A
//! revocation the directory refuses, a chain it already revoked or one past its validity, is done
//! and its answer kept; one it could not be asked is asked again at the issuer's next look.

use crate::App;
use crate::error::AppError;
use crate::ports::{AcmeError, CertificateWrite as _, Clock, RecipeSource, Store, Tx as _};

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Revokes each exposed chain waiting to be; how many were answered.
    pub async fn revoke_exposed(&self) -> Result<u64, AppError> {
        let mut tx = self.store.begin().await?;
        let pending = tx.pending_revocations().await?;
        tx.commit().await?;
        let Some(acme) = self.acme.clone().filter(|_| !pending.is_empty()) else { return Ok(0) };
        let mut answered = 0;
        for r in pending {
            let outcome = match acme.revoke(&r.chain).await {
                Ok(()) => "revoked".to_owned(),
                Err(AcmeError::Refused(why)) => format!("refused: {why}"),
                Err(e @ (AcmeError::Unavailable(_) | AcmeError::Terms(_))) => {
                    return Err(AppError::Unavailable {
                        what: "the ACME directory",
                        detail: e.to_string(),
                    });
                }
            };
            let mut tx = self.store.begin().await?;
            tx.record_revocation(&r, &outcome, self.clock.now()).await?;
            tx.commit().await?;
            answered += 1;
        }
        Ok(answered)
    }
}
