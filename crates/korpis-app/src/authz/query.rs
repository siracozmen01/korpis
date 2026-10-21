// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading grants.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged, visible};
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
    /// Those issued from a template, at any version: each one the caller holds or may read
    /// under `grant.read` over its scope (§3.5 of docs/08-identity.md).
    FromTemplate(Uuid),
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
    /// One grant with its chain, if the caller holds it or may read grants over its scope. A
    /// share link's holder reads what the link grants and not the chain above it: the link was
    /// sent to someone with no account, and who issued it, holding what, is not theirs to read
    /// (§6.1 of docs/08-identity.md).
    pub async fn get_grant(&self, caller: &Caller, id: Uuid) -> Result<GrantWithChain, AppError> {
        caller.direct()?;
        let mut records = self.store.grant_chain(id).await?;
        if records.is_empty() {
            return Err(AppError::NotFound { kind: "grant" });
        }
        let grant = records.remove(0);
        let anchor = grant.grant.scope.anchor();
        let holds = grant.grant.subject == caller.subject
            && self.reached_each(caller, &[anchor]).await?.first() == Some(&true);
        if !holds && !self.permitted(caller, "grant.read", anchor).await? {
            return Err(AppError::NotFound { kind: "grant" });
        }
        if holds && matches!(caller.principal, Principal::Link(_)) {
            records.clear();
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
        caller.direct()?;
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
                return self.grants_held(caller, page).await;
            }
            GrantsFilter::FromTemplate(template) => {
                return self.grants_from_template(caller, template, page).await;
            }
        };
        let next = page.next(&items, |g| g.grant.id);
        Ok(Paged { items, next })
    }

    /// The grants the caller holds, those beneath its portal's organization alone for a session
    /// begun at one.
    async fn grants_held(
        &self,
        caller: &Caller,
        page: Page,
    ) -> Result<Paged<GrantRecord>, AppError> {
        let read =
            |p: Page| async move { Ok(self.store.grants_held(&caller.subject.key, p).await?) };
        let reached = |found: Vec<GrantRecord>| async move {
            let anchors: Vec<ObjectRef> = found.iter().map(|g| g.grant.scope.anchor()).collect();
            let shown = self.reached_each(caller, &anchors).await?;
            Ok(found.into_iter().zip(shown).filter_map(|(g, s)| s.then_some(g)).collect())
        };
        visible(page, |g| g.grant.id, read, reached).await
    }

    /// The grants issued from `template` that the caller holds or may read.
    async fn grants_from_template(
        &self,
        caller: &Caller,
        template: Uuid,
        page: Page,
    ) -> Result<Paged<GrantRecord>, AppError> {
        let read = |p: Page| async move { Ok(self.store.grants_from_template(template, p).await?) };
        let readable = |found: Vec<GrantRecord>| async move {
            let anchors: Vec<ObjectRef> = found.iter().map(|g| g.grant.scope.anchor()).collect();
            let shown = self.permitted_each(caller, "grant.read", &anchors).await?;
            let reached = self.reached_each(caller, &anchors).await?;
            Ok(found
                .into_iter()
                .zip(shown.into_iter().zip(reached))
                .filter_map(|(g, (s, r))| {
                    (s || (r && g.grant.subject == caller.subject)).then_some(g)
                })
                .collect())
        };
        visible(page, |g| g.grant.id, read, readable).await
    }
}
