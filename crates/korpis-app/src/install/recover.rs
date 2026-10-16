// SPDX-License-Identifier: AGPL-3.0-or-later

//! Local root recovery (§9.2 of docs/08-identity.md): whoever holds root on the machine running
//! the control plane already holds everything, so recovering a lost root grant is done there and
//! says so. It issues a new root grant to a new account and an invitation only registering a
//! passkey redeems, and records it as the `System` bootstrap actor and the operating-system user
//! who ran it.

use super::{RootInvitation, root_account};
use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, EffectWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Issues a new root grant over the root organization to a new account, invited for an hour,
    /// at the machine, by `operator`, the operating-system user who asked.
    pub async fn recover_root(&self, operator: &str) -> Result<RootInvitation, AppError> {
        let bootstrap = Principal::System(SystemActor::Bootstrap);
        let now = self.clock.now();
        let roots = self.store.organizations(None, Page { after: None, size: 1 }).await?;
        let org = roots
            .into_iter()
            .next()
            .ok_or_else(|| AppError::conflict("install.missing", "nothing is installed here"))?;
        let mut tx = self.store.begin().await?;
        let invitation = root_account(&mut tx, &org, "root (recovered)", &bootstrap, now).await?;
        let recovered = Effect::new(bootstrap, "root.recovered", Outcome::Succeeded)
            .under(invitation.grant)
            .after(details([
                ("account", invitation.account.to_string().into()),
                ("grant", invitation.grant.to_string().into()),
                ("operator", operator.into()),
            ]));
        tx.append_effect(&recovered).await?;
        tx.commit().await?;
        Ok(invitation)
    }
}
