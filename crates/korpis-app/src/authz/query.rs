// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading grants.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{GrantRecord, ObjectRef};
use korpis_core::principal::Principal;
use uuid::Uuid;

/// Which grants to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantsFilter {
    /// The caller's own.
    Mine,
    /// Those anchored at an object; needs `grant.read` there.
    On(ObjectRef),
    /// Those held by a principal; only the caller's own may be listed this way.
    HeldBy(Principal),
}

/// A grant and the chain above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantWithChain {
    /// The grant.
    pub grant: GrantRecord,
    /// Its ancestors, parent first, root last.
    pub chain: Vec<GrantRecord>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One grant with its chain, if the caller holds it or may read grants over its scope.
    pub async fn get_grant(&self, caller: &Caller, id: Uuid) -> Result<GrantWithChain, AppError> {
        let mut records = self.store.grant_chain(id).await?;
        if records.is_empty() {
            return Err(AppError::NotFound { kind: "grant" });
        }
        let grant = records.remove(0);
        if grant.grant.subject != caller.subject
            && !self.permitted(caller, "grant.read", grant.grant.scope.anchor()).await?
        {
            return Err(AppError::NotFound { kind: "grant" });
        }
        Ok(GrantWithChain { grant, chain: records })
    }

    /// Grants by `filter`.
    pub async fn list_grants(
        &self,
        caller: &Caller,
        filter: GrantsFilter,
        page: Page,
    ) -> Result<Paged<GrantRecord>, AppError> {
        let items = match filter {
            GrantsFilter::On(object) => {
                self.authorize(caller, "grant.read", object).await?;
                self.store.grants_on(object, page).await?
            }
            GrantsFilter::HeldBy(p) if p.subject() != caller.subject => {
                return Err(AppError::invalid(
                    "subject",
                    "list another subject's grants by scope, with grant.read",
                ));
            }
            GrantsFilter::HeldBy(_) | GrantsFilter::Mine => {
                self.store.grants_held(&caller.subject.key, page).await?
            }
        };
        let next = page.next(&items, |g| g.grant.id);
        Ok(Paged { items, next })
    }
}
