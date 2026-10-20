// SPDX-License-Identifier: AGPL-3.0-or-later

//! Delegation ceilings (§5.1 of docs/08-identity.md): the longest a grant beneath a set subject
//! lasts, counted from its issue. Seven days where no organization says; the root organization
//! sets the installation's, and any organization lowers it for its own subtree. Authorization
//! takes the lowest along a scope's organizations at every use.

use crate::App;
use crate::authz::Snapshot;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite as _, RecipeSource, Store, TenancyWrite as _};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::binding::{DELEGATION_CEILING, ceiling_at};
use korpis_core::authz::{Grant, Kind, ObjectRef};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const OPERATION: &str = "tenancy.set_delegation_ceiling";

/// The shortest ceiling: a delegation that ends sooner than this is not one anybody can use.
pub const MIN_CEILING: Duration = Duration::from_secs(60 * 60);
/// The longest the root organization may raise it to: a bound on how long a delegation can
/// outlive the membership it came from, which is what the ceiling is for.
pub const MAX_CEILING: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// The ceiling at an organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ceiling {
    /// The one it sets itself, if it does.
    pub own: Option<Duration>,
    /// The one in force there: the lowest along its organizations, or the default.
    pub effective: Duration,
}

fn org(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The ceiling at `organization`, under `org.read` there.
    pub async fn delegation_ceiling(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<Ceiling, AppError> {
        self.authorize(caller, "org.read", org(organization)).await?;
        self.ceiling_here(organization).await
    }

    async fn ceiling_here(&self, organization: Uuid) -> Result<Ceiling, AppError> {
        let ancestry = self.store.ancestry(org(organization)).await?;
        let own = ancestry.first().and_then(|n| n.ceiling);
        let effective = ancestry.iter().filter_map(|n| n.ceiling).min();
        Ok(Ceiling { own, effective: effective.unwrap_or(DELEGATION_CEILING) })
    }

    /// Sets the ceiling `organization` sets for its subtree, or removes it with `None`, under
    /// `org.write` there. Below the root it may only lower what its parent's subtree has, so
    /// only the root raises it, as an effect like every change.
    pub async fn set_delegation_ceiling(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Ceiling>,
        (organization, ceiling): (Uuid, Option<Duration>),
    ) -> Result<Reply<Ceiling>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let permit = self.authorize(caller, "org.write", org(organization)).await?;
        let ancestry = self.store.ancestry(org(organization)).await?;
        let above = ancestry.iter().skip(1).filter_map(|n| n.ceiling).min();
        let bound =
            if ancestry.len() > 1 { above.unwrap_or(DELEGATION_CEILING) } else { MAX_CEILING };
        if let Some(c) = ceiling
            && !(MIN_CEILING..=bound).contains(&c)
        {
            let why =
                format!("between an hour and {}s, what this organization may set", bound.as_secs());
            return Err(AppError::invalid("ceiling", why));
        }
        let mut tx = self.store.begin().await?;
        let replaced = tx
            .set_delegation_ceiling(organization, ceiling)
            .await?
            .ok_or(AppError::NotFound { kind: "organization" })?;
        let secs = |d: Option<Duration>| d.map(|d| d.as_secs());
        let effect = caller
            .effect("org.delegation_ceiling.set", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("organization", organization.to_string().into()),
                ("ceiling_seconds", secs(ceiling).into()),
                ("replaced_seconds", secs(replaced).into()),
            ]));
        tx.append_effect(&effect).await?;
        let effective = ceiling.into_iter().chain(above).min();
        let set = Ceiling { own: ceiling, effective: effective.unwrap_or(DELEGATION_CEILING) };
        finish(tx, caller, idem, OPERATION, set).await
    }

    /// Clamps `child`'s expiry to the ceiling it is issued under when a set subject holds a
    /// grant on the chain above it, whatever was asked, so what a grant shows is what
    /// evaluation enforces (K-3).
    pub(crate) async fn clamp_to_ceiling(
        &self,
        child: &mut Grant,
        now: SystemTime,
    ) -> Result<(), AppError> {
        let Some(parent) = child.parent else { return Ok(()) };
        let chain = self.store.grant_chain(parent).await?;
        if !chain.iter().any(|r| r.grant.subject.set) {
            return Ok(());
        }
        let mut snapshot = Snapshot::default();
        snapshot.load(&self.store, child.scope.anchor()).await?;
        let ceiling = ceiling_at(&snapshot, child.scope.anchor());
        let until = now.checked_add(ceiling).ok_or(AppError::invalid("ceiling", "too long"))?;
        child.conditions.expires_at =
            Some(child.conditions.expires_at.map_or(until, |e| e.min(until)));
        Ok(())
    }
}
