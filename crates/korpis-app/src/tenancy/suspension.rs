// SPDX-License-Identifier: AGPL-3.0-or-later

//! Suspending and resuming an organization (§2.1 of docs/08-identity.md): the reversible lever a
//! host pulls for non-payment. It is one row: every grant scoped in the organization's subtree is
//! suspended by derivation at use, and, when the suspension stops the workloads, every workload
//! beneath it is told stopped by the same derivation; resuming restores them unchanged, since
//! nothing was written onto any of them. Both are effective only from above, so each is
//! authorized over the organization's parent, which a grant scoped at the organization itself
//! never covers: a tenant cannot resume itself, and a root organization, which has nothing above
//! it, is suspended by no one.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, TenancyWrite};
use korpis_core::audit::{Outcome, details};
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

/// The conflict of a change made to an organization read at a version it no longer has.
fn changed() -> AppError {
    let why = "the organization changed while this was decided; read it again and retry";
    AppError::conflict("organization.changed", why)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organization to suspend or resume, and its parent, which the change is authorized
    /// over; a root is refused once the caller has shown it may see it.
    async fn above(
        &self,
        caller: &Caller,
        id: Uuid,
        action: &str,
    ) -> Result<(Organization, Uuid), AppError> {
        let target = self.store.organization(id).await?;
        let Some((target, parent)) = target.and_then(|o| o.parent.map(|p| (o, p))) else {
            self.authorize(caller, action, org(id)).await?;
            let why = "a root organization has nothing above it to suspend or resume it";
            return Err(AppError::invalid("organization", why));
        };
        Ok((target, parent))
    }

    /// The nearest organization at or above `organization` that is suspended with its workloads
    /// stopped: while there is one, every workload beneath it is told stopped, whatever power it
    /// declares (§2.1 of docs/08-identity.md). Read at every computation, like the suspension of
    /// grants, so a suspension and its resumption reach every workload beneath at once, nested
    /// suspensions included.
    pub(crate) async fn held_by(&self, organization: Uuid) -> Result<Option<Uuid>, AppError> {
        for node in self.store.ancestry(org(organization)).await? {
            if !node.suspended {
                continue;
            }
            let status = self.store.organization(node.object.id).await?.map(|o| o.status);
            if let Some(OrganizationStatus::Suspended(s)) = status
                && s.on_suspend == OnSuspend::Stop
            {
                return Ok(Some(node.object.id));
            }
        }
        Ok(None)
    }

    /// Suspends an organization, from a strict ancestor; stopping its workloads as well takes
    /// the authority to stop them.
    pub async fn suspend_organization(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Organization>,
        cmd: SuspendOrganization,
    ) -> Result<Reply<Organization>, AppError> {
        if let Some(r) = self.replay(caller, idem, SUSPEND).await? {
            return Ok(Reply::Replayed(r));
        }
        let (target, parent) = self.above(caller, cmd.organization, "org.suspend").await?;
        let permit = self.authorize(caller, "org.suspend", org(parent)).await?;
        let reason = cmd.reason.trim().to_owned();
        if reason.is_empty() || reason.chars().count() > REASON_MAX {
            let why =
                format!("say why, in at most {REASON_MAX} characters; the tenant is shown it");
            return Err(AppError::invalid("reason", why));
        }
        if matches!(target.status, OrganizationStatus::Suspended(_)) {
            let why = "the organization is already suspended";
            return Err(AppError::conflict("organization.suspended", why));
        }
        let stops = cmd.on_suspend == OnSuspend::Stop;
        if stops {
            self.authorize(caller, "workload.stop", org(target.id)).await?;
        }
        let suspension = Suspension {
            by: caller.principal.clone(),
            at: self.clock.now(),
            reason: reason.clone(),
            on_suspend: cmd.on_suspend,
        };
        let mut tx = self.store.begin().await?;
        let status = OrganizationStatus::Suspended(suspension);
        let written = tx.change_organization_status(target.id, target.version, &status).await?;
        let suspended = written.ok_or_else(changed)?;
        let effect = caller
            .effect("org.suspend", Outcome::Succeeded)
            .under(permit.grant)
            .about(org(target.id))
            .after(details([
                ("organization", cmd.organization.to_string().into()),
                ("reason", reason.into()),
                ("on_suspend", if stops { "stop" } else { "keep_running" }.into()),
            ]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, SUSPEND, suspended).await
    }

    /// Resumes a suspended organization, from a strict ancestor: every grant its suspension held
    /// authorizes again, as it was, and every workload it held stopped is told its own declared
    /// power again, which takes the authority to start them.
    pub async fn resume_organization(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Organization>,
        organization: Uuid,
    ) -> Result<Reply<Organization>, AppError> {
        if let Some(r) = self.replay(caller, idem, RESUME).await? {
            return Ok(Reply::Replayed(r));
        }
        let (target, parent) = self.above(caller, organization, "org.resume").await?;
        let permit = self.authorize(caller, "org.resume", org(parent)).await?;
        let OrganizationStatus::Suspended(suspension) = &target.status else {
            let why = "the organization is not suspended";
            return Err(AppError::conflict("organization.not_suspended", why));
        };
        if suspension.on_suspend == OnSuspend::Stop {
            self.authorize(caller, "workload.start", org(target.id)).await?;
        }
        let mut tx = self.store.begin().await?;
        let active = OrganizationStatus::Active;
        let written = tx.change_organization_status(target.id, target.version, &active).await?;
        let resumed = written.ok_or_else(changed)?;
        let effect = caller
            .effect("org.resume", Outcome::Succeeded)
            .under(permit.grant)
            .about(org(target.id))
            .after(details([("organization", organization.to_string().into())]));
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, RESUME, resumed).await
    }
}
