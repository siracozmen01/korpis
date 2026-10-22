// SPDX-License-Identifier: AGPL-3.0-or-later

//! The organization a signed-in caller lands at (§7 and question 2 of §10 of
//! docs/11-surface-web.md, Finding 134 of docs/23-walkthroughs.md): whoever provides their
//! access, whose theme they wear and whose layout their landing shows. At a portal it is the
//! portal's organization, the address being that organization's; elsewhere it is the nearest
//! organization above everything the caller's grants reach, which the control plane can name
//! from the grants it holds though the caller may read none of those organizations, as a
//! customer holding only a project's grant reads none; and with no grant, the installation's
//! root. A share link's holder lands nowhere, a link opening one place (§6 of
//! docs/11-surface-web.md).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::principal::Principal;
use uuid::Uuid;

/// The nearest organization every chain holds, each chain an object's organizations from the
/// nearest up to its root; none if `chains` is empty or they share none.
fn nearest_common(chains: &[Vec<Uuid>]) -> Option<Uuid> {
    let (first, rest) = chains.split_first()?;
    first.iter().copied().find(|org| rest.iter().all(|c| c.contains(org)))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organization `caller` lands at, reached at `portal` or at the installation's own
    /// address; `None` only where the installation has no organization yet.
    pub(crate) async fn landing_organization(
        &self,
        caller: &Caller,
        portal: Option<Uuid>,
    ) -> Result<Option<Uuid>, AppError> {
        if matches!(caller.principal, Principal::Link(_) | Principal::Anonymous) {
            return Err(AppError::Unauthenticated);
        }
        if let Some(p) = portal {
            return self.portal_organization(p).await;
        }
        let mut chains = Vec::new();
        let full = usize::try_from(Page::MAX).unwrap_or(usize::MAX);
        let mut page = Page { after: None, size: Page::MAX };
        let holding = caller.holding();
        loop {
            let found = self.store.grants_held(&holding, page).await?;
            for held in found.iter().filter(|g| g.revoked_at.is_none()) {
                chains.push(self.organizations_above(held.grant.scope.anchor()).await?);
            }
            match found.last() {
                Some(last) if found.len() == full => {
                    page = Page { after: Some(last.grant.id), size: Page::MAX };
                }
                _ => break,
            }
        }
        // A grant on an object since deleted names no organization, and bounds nothing.
        chains.retain(|c| !c.is_empty());
        let nearest = match nearest_common(&chains) {
            Some(org) => Some(org),
            None => self.store.installation_root().await?,
        };
        self.within_reach(caller, nearest).await
    }

    /// `nearest`, unless `caller` is a session confined beneath an organization it does not lie
    /// beneath, as one whose secrets were set at a portal may be (§9.8 of docs/08-identity.md):
    /// it reaches nothing above that organization, which then provides what it reaches.
    async fn within_reach(
        &self,
        caller: &Caller,
        nearest: Option<Uuid>,
    ) -> Result<Option<Uuid>, AppError> {
        let (Some(within), Some(org)) = (caller.within, nearest) else {
            return Ok(caller.within.or(nearest));
        };
        let above = self.store.ancestry(ObjectRef::new(Kind::Organization, org)).await?;
        let beneath = above.iter().any(|n| n.object == ObjectRef::new(Kind::Organization, within));
        Ok(Some(if beneath { org } else { within }))
    }
}

#[cfg(test)]
mod tests {
    use super::nearest_common;
    use uuid::Uuid;

    #[test]
    fn the_landing_is_the_nearest_organization_above_every_grant() {
        let [root, northwind, blockhaven, pinecone] = [1, 2, 3, 4].map(Uuid::from_u128);
        let project_in_blockhaven = vec![blockhaven, northwind, root];
        assert_eq!(nearest_common(std::slice::from_ref(&project_in_blockhaven)), Some(blockhaven));
        let northwind_itself = vec![northwind, root];
        let both = [project_in_blockhaven.clone(), northwind_itself];
        assert_eq!(nearest_common(&both), Some(northwind), "the provider of both");
        let elsewhere = [project_in_blockhaven, vec![pinecone, root]];
        assert_eq!(nearest_common(&elsewhere), Some(root), "nothing nearer serves both");
        assert_eq!(nearest_common(&[]), None, "no grant names no organization");
    }
}
