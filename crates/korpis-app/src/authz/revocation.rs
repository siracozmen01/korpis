// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list (§6.3 of docs/08-identity.md), computed whole from current state: a grant
//! revoked, or suspended and not lifted, within the horizon is on it, and a suspension lifted is
//! off it by the next computation. The horizon allows the clock skew the agent session allows,
//! because a verifier's clock may run that far behind the control plane's.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::revocation::{RevocationList, SignedList, horizon};

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The revocation list as of now, signed by the token key.
    pub async fn revocation_list(&self) -> Result<SignedList, AppError> {
        let now = self.clock.now();
        let since = horizon(now, self.session_terms.auth_skew);
        let grants = self.store.revoked_since(since).await?;
        Ok(RevocationList::new(now, grants).sign(&self.token_key))
    }
}
