// SPDX-License-Identifier: AGPL-3.0-or-later

//! Verifying a domain by its TXT record, deleting one with what was delegated beneath it, and
//! reading them.

use super::{held, routed};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged, visible};
use crate::ports::{Clock, DomainWrite as _, EffectWrite as _, LookupError, RecipeSource, Store};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::dns::domain::{Domain, Verification, blocked_by, holder_of};
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A domain, authorized for `action`.
    async fn domain_for(
        &self,
        caller: &Caller,
        action: &str,
        id: Uuid,
    ) -> Result<(Domain, Uuid), AppError> {
        let permit = self.authorize(caller, action, ObjectRef::new(Kind::Domain, id)).await?;
        let d = self.store.domain(id).await?.ok_or(AppError::NotFound { kind: "domain" })?;
        Ok((d, permit.grant))
    }

    /// Whether the TXT record of `d` holds its token, asked of public DNS.
    async fn token_published(&self, d: &Domain) -> Result<bool, AppError> {
        let lookup = self.lookup.as_ref().ok_or_else(|| AppError::Unavailable {
            what: "DNS",
            detail: "the control plane was started without a resolver".into(),
        })?;
        match lookup.txt(&d.name.verification_record()).await {
            Ok(found) => Ok(found.iter().any(|t| t.trim() == d.token)),
            Err(LookupError::NotFound) => Ok(false),
            Err(LookupError::Failed(detail)) => Err(AppError::Unavailable { what: "DNS", detail }),
        }
    }

    /// Verifies a domain by its TXT record; one verified already is answered as it is.
    pub async fn verify_domain(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Domain>,
        id: Uuid,
    ) -> Result<Reply<Domain>, AppError> {
        const OPERATION: &str = "domain.verify";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let (mut d, grant) = self.domain_for(caller, "domain.write", id).await?;
        if d.verified() || d.verification != Verification::DnsTxt {
            return Ok(Reply::Fresh(d));
        }
        if !self.token_published(&d).await? {
            let why = format!("no TXT record at {} holds the token", d.name.verification_record());
            return Err(AppError::conflict("domain.unverified", why));
        }
        let mut tx = self.store.begin().await?;
        tx.lock_domains().await?;
        let others = tx.verified_overlapping(&d.name).await?;
        if blocked_by(&d, &others, &[]).is_some() {
            return Err(held());
        }
        let now = self.clock.now();
        tx.verify_domain(d.id, now).await?;
        d.verified_at = Some(now);
        let effect =
            caller.effect("domain.verify", Outcome::Succeeded).under(grant).after(details([
                ("domain", d.id.to_string().into()),
                ("name", d.name.as_str().into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, d).await
    }

    /// Deletes a domain, and every domain delegated beneath it; how many went.
    pub async fn delete_domain(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, u64>,
        id: Uuid,
    ) -> Result<Reply<u64>, AppError> {
        const OPERATION: &str = "domain.delete";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let (d, grant) = self.domain_for(caller, "domain.write", id).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_domains().await?;
        // Every route beneath it must keep its holder once it and its delegations are gone.
        let tree = tx.delegation_tree(d.id).await?;
        for r in tx.routes_beneath(&d.name).await? {
            let mut covering = tx.verified_overlapping(&r.name).await?;
            covering.retain(|c| !tree.contains(&c.id));
            if holder_of(&r.name, &covering) != Some(r.organization) {
                return Err(routed(&r.name));
            }
        }
        let gone = tx.delete_domain(d.id).await?;
        let effect =
            caller.effect("domain.delete", Outcome::Succeeded).under(grant).after(details([
                ("domain", d.id.to_string().into()),
                ("name", d.name.as_str().into()),
                ("deleted", gone.into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, gone).await
    }

    /// One domain the caller may read.
    pub async fn get_domain(&self, caller: &Caller, id: Uuid) -> Result<Domain, AppError> {
        Ok(self.domain_for(caller, "domain.read", id).await?.0)
    }

    /// An organization's domains the caller may read.
    pub async fn list_domains(
        &self,
        caller: &Caller,
        organization: Uuid,
        page: Page,
    ) -> Result<Paged<Domain>, AppError> {
        let read = |p: Page| async move { Ok(self.store.domains_of(organization, p).await?) };
        let readable = |found: Vec<Domain>| async move {
            let objects: Vec<ObjectRef> =
                found.iter().map(|d| ObjectRef::new(Kind::Domain, d.id)).collect();
            let shown = self.permitted_each(caller, "domain.read", &objects).await?;
            Ok(found.into_iter().zip(shown).filter_map(|(d, s)| s.then_some(d)).collect())
        };
        visible(page, |d| d.id, read, readable).await
    }
}
