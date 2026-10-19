// SPDX-License-Identifier: AGPL-3.0-or-later

//! Suspending and resuming an organization (§2.1 of docs/08-identity.md): the reversible lever a
//! host pulls for non-payment. It is one row: every grant scoped in the organization's subtree is
//! suspended by derivation at use, and resuming restores them unchanged. Both are effective only
//! from above, so each is authorized over the organization's parent, which a grant scoped at the
//! organization itself never covers: a tenant cannot resume itself, and a root organization,
//! which has nothing above it, is suspended by no one.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, TenancyWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::tenancy::{OnSuspend, Organization, OrganizationStatus, Suspension};
use uuid::Uuid;

const SUSPEND: &str = "tenancy.suspend_organization";
const RESUME: &str = "tenancy.resume_organization";

/// The longest reason a suspension records; it is shown to the tenant.
const REASON_MAX: usize = 500;

/// An organization to suspend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendOrganization {
    /// The organization.
    pub organization: Uuid,
    /// Why, as the tenant will be shown it.
    pub reason: String,
    /// What happens to its workloads.
    pub on_suspend: OnSuspend,
}

fn org(id: Uuid) -> ObjectRef {
    ObjectRef::new(Kind::Organization, id)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organization to suspend or resume, and its parent, which the change is authorized
    /// over; a root is refused once the caller has shown it may see it.
    async fn above(&self, caller: &Caller, id: Uuid, action: &str) -> Result<Uuid, AppError> {
        let target = self.store.organization(id).await?;
        let Some(parent) = target.as_ref().and_then(|o| o.parent) else {
            self.authorize(caller, action, org(id)).await?;
            let why = "a root organization has nothing above it to suspend or resume it";
            return Err(AppError::invalid("organization", why));
        };
        Ok(parent)
    }

    /// Suspends an organization, from a strict ancestor.
    pub async fn suspend_organization(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Organization>,
        cmd: SuspendOrganization,
    ) -> Result<Reply<Organization>, AppError> {
        if let Some(r) = self.replay(caller, idem, SUSPEND).await? {
            return Ok(Reply::Replayed(r));
        }
        let parent = self.above(caller, cmd.organization, "org.suspend").await?;
        let permit = self.authorize(caller, "org.suspend", org(parent)).await?;
        let reason = cmd.reason.trim().to_owned();
        if reason.is_empty() || reason.chars().count() > REASON_MAX {
            let why =
                format!("say why, in at most {REASON_MAX} characters; the tenant is shown it");
            return Err(AppError::invalid("reason", why));
        }
        if cmd.on_suspend == OnSuspend::Stop {
            let why = "stopping a suspended organization's workloads is not built yet: suspend \
                       it keeping them running";
            return Err(AppError::invalid("on_suspend", why));
        }
        let suspension = Suspension {
            by: caller.principal.clone(),
            at: self.clock.now(),
            reason: reason.clone(),
            on_suspend: cmd.on_suspend,
        };
        let mut tx = self.store.begin().await?;
        let status = OrganizationStatus::Suspended(suspension);
        let changed = tx.change_organization_status(cmd.organization, false, &status).await?;
        let suspended = changed.ok_or_else(|| {
            AppError::conflict("organization.suspended", "the organization is already suspended")
        })?;
        let effect = Effect::new(caller.principal.clone(), "org.suspend", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("organization", cmd.organization.to_string().into()),
                ("reason", reason.into()),
                ("on_suspend", "keep_running".into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, SUSPEND, suspended).await
    }

    /// Resumes a suspended organization, from a strict ancestor: every grant its suspension held
    /// authorizes again, as it was.
    pub async fn resume_organization(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Organization>,
        organization: Uuid,
    ) -> Result<Reply<Organization>, AppError> {
        if let Some(r) = self.replay(caller, idem, RESUME).await? {
            return Ok(Reply::Replayed(r));
        }
        let parent = self.above(caller, organization, "org.resume").await?;
        let permit = self.authorize(caller, "org.resume", org(parent)).await?;
        let mut tx = self.store.begin().await?;
        let changed =
            tx.change_organization_status(organization, true, &OrganizationStatus::Active).await?;
        let resumed = changed.ok_or_else(|| {
            AppError::conflict("organization.not_suspended", "the organization is not suspended")
        })?;
        let effect = Effect::new(caller.principal.clone(), "org.resume", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([("organization", organization.to_string().into())]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, RESUME, resumed).await
    }
}
