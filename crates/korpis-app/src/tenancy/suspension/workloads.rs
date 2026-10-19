// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a suspension does to the workloads beneath it when it says `stop` (§2.1 of
//! docs/08-identity.md), and what resuming does to them: a power plan for each, under the grant
//! of whoever suspended or resumed, recorded as that person's act and visible to the tenant. The
//! suspension is the decision a stop plan would otherwise ask a person to confirm, so a plan that
//! needs only confirmation is approved by it; one that needs an approver's authority still waits
//! for that approver. Resuming starts again exactly the workloads the suspension stopped whose
//! power no one has changed since.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, RecipeSource, Store, WorkloadFilter};
use crate::workload::Prepared;
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::plan::Approval;
use korpis_core::workload::{PowerChange, PowerState, Workload};
use uuid::Uuid;

/// The power changes a suspension or a resumption makes, and the grant they are made under.
pub(super) struct Changes {
    /// The grant of whoever made them.
    pub(super) grant: Uuid,
    /// Each one, prepared.
    pub(super) prepared: Vec<Prepared>,
}

fn confirmed_by(p: &mut Prepared, why: &str) {
    if p.plan.approval == Approval::Confirmation {
        p.plan.approval = Approval::Auto;
        why.clone_into(&mut p.plan.approval_reason);
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Every live workload beneath organization `organization`, its own and its descendants'.
    async fn beneath(&self, organization: Uuid) -> Result<Vec<Workload>, AppError> {
        let filter = WorkloadFilter { beneath: Some(organization), ..WorkloadFilter::default() };
        let mut all = Vec::new();
        let mut page = Page::new(None, Page::MAX);
        loop {
            let rows = self.store.workloads(&filter, page).await?;
            let next = page.next(&rows, |w| w.id);
            all.extend(rows);
            match next {
                Some(after) => page = Page::new(Some(after), Page::MAX),
                None => return Ok(all),
            }
        }
    }

    /// The stops a suspension of `organization` makes: one for each workload beneath it that is
    /// declared running, under the suspender's `workload.stop` over the organization.
    pub(super) async fn stops(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<Changes, AppError> {
        let object = ObjectRef::new(Kind::Organization, organization);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.stop", object).await?;
        let mut prepared = Vec::new();
        for w in self.beneath(organization).await? {
            let mut p =
                self.prepare_power(caller, w, PowerChange::Stop, (&permit, &proposing)).await?;
            if p.head.desired == PowerState::Stopped {
                continue;
            }
            confirmed_by(&mut p, "its organization's suspension confirmed it");
            prepared.push(p);
        }
        Ok(Changes { grant: permit.grant, prepared })
    }

    /// The starts resuming `organization` makes: one for each workload its suspension stopped
    /// whose power is still what the suspension declared, under the resumer's `workload.start`
    /// over the organization; `None` when the suspension stopped nothing.
    pub(super) async fn restarts(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<Option<Changes>, AppError> {
        let stopped = self.store.suspension_stops(organization).await?;
        if stopped.is_empty() {
            return Ok(None);
        }
        let object = ObjectRef::new(Kind::Organization, organization);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.start", object).await?;
        let mut prepared = Vec::new();
        for (id, version) in stopped {
            let live = self.store.workload(id).await?.filter(|w| w.deleted_at.is_none());
            let Some(w) = live.filter(|w| w.power_version == version) else { continue };
            let mut p =
                self.prepare_power(caller, w, PowerChange::Start, (&permit, &proposing)).await?;
            confirmed_by(&mut p, "its organization's resumption confirmed it");
            prepared.push(p);
        }
        Ok(Some(Changes { grant: permit.grant, prepared }))
    }
}
