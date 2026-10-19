// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing to an external identity (§5 of docs/08-identity.md): a grant names one only through a
//! live identity binding, whose provider vouches only at and beneath the organization it is
//! installed into. A set's grants come from the template its binding names, and without one may
//! not delegate (§3.4); whether they may carry irreversible actions is the binding's to say
//! (§5.2), never the request's.

use crate::App;
use crate::authz::snapshot::Snapshot;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Grant, GrantRecord};
use korpis_core::principal::Principal;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Checks a grant to an external identity against the binding vouching for it, and records
    /// on it what that binding allows. Any other holder passes untouched.
    pub(crate) async fn vouched(&self, record: &mut GrantRecord) -> Result<(), AppError> {
        if !matches!(record.holder, Principal::ExternalUser { .. } | Principal::ExternalSet { .. })
        {
            return Ok(());
        }
        let binding = self.store.binding_of(&record.holder.key()).await?.ok_or_else(|| {
            let why = "an external identity is named only through a live identity binding";
            AppError::invalid("subject", why)
        })?;
        let mut snapshot = Snapshot::default();
        snapshot.load_grant(&self.store, &record.grant).await?;
        if !binding.provider.reaches(&record.grant.scope, &snapshot) {
            let why = "its identity provider vouches only at and beneath where it is installed";
            return Err(AppError::invalid("scope", why));
        }
        if binding.is_set() {
            match binding.terms.template {
                Some(t) if record.from_template != Some(t) => {
                    let why = "a grant to this set is issued from its binding's template, at the \
                               version the binding names";
                    return Err(AppError::invalid("template", why));
                }
                None if record.grant.delegation.allowed => {
                    let why = "a grant to a set delegates only as its binding's template says";
                    return Err(AppError::invalid("delegation", why));
                }
                _ => {}
            }
            record.grant.irreversible_override = binding.terms.allows_irreversible;
        }
        Ok(())
    }

    /// Whether the binding of `child`'s subject lets it carry irreversible actions: only a live
    /// set binding an operator declared so.
    pub(crate) async fn binding_allows_irreversible(
        &self,
        child: &Grant,
    ) -> Result<bool, AppError> {
        if !child.subject.set {
            return Ok(false);
        }
        let binding = self.store.binding_of(&child.subject.key).await?;
        Ok(binding.is_some_and(|b| b.terms.allows_irreversible))
    }
}
