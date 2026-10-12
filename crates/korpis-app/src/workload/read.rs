// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading workloads (§4.3 of docs/02-architecture.md): everything a client needs to render one
//! honestly, and the intent chain. Existence is gated by read authority (§8 of docs/10-api.md).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store, WorkloadFilter};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::labels::LabelSelector;
use korpis_core::workload::status::{Convergence, active_plan, convergence};
use korpis_core::workload::{Intent, Plan, PowerRecord, Workload};
use uuid::Uuid;

/// One workload as a client renders it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkloadView {
    /// The workload.
    pub workload: Workload,
    /// Its newest power declaration.
    pub power: PowerRecord,
    /// The newest declared intent.
    pub head: Intent,
    /// The approved intent, once there is one.
    pub target: Option<Intent>,
    /// The plan proposed, approved, or applying, if any.
    pub active_plan: Option<Plan>,
    /// How far it is from what was approved.
    pub convergence: Convergence,
}

/// Which workloads to list: in a project or under an organization, matching a selector.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListWorkloads {
    /// In this project.
    pub project: Option<Uuid>,
    /// Owned by this organization.
    pub organization: Option<Uuid>,
    /// Matching these labels.
    pub selector: LabelSelector,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    async fn view(&self, workload: Workload) -> Result<WorkloadView, AppError> {
        let id = workload.id;
        let missing = |what: &str| AppError::Internal(format!("workload {id} has no {what}"));
        let power = self.store.power(id).await?.ok_or_else(|| missing("power"))?;
        let head = self.store.intent(workload.intent).await?.ok_or_else(|| missing("head"))?;
        let target = match workload.target {
            Some(t) => Some(self.store.intent(t).await?.ok_or_else(|| missing("target"))?),
            None => None,
        };
        let open = self.store.open_plans(id).await?;
        let active = active_plan(&open).cloned();
        let placed = self.store.placed(id).await?;
        let convergence = convergence(&workload, &open, placed, self.clock.now());
        Ok(WorkloadView { workload, power, head, target, active_plan: active, convergence })
    }

    /// One workload the caller may read, deleted or not.
    pub async fn get_workload(&self, caller: &Caller, id: Uuid) -> Result<WorkloadView, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, id)).await?;
        let w = self.store.workload(id).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        self.view(w).await
    }

    /// The workloads of a project, or under an organization, that the caller may read. A page
    /// may hold fewer than its size when the selector skips some; its cursor still continues
    /// after the last workload read, so no workload is skipped.
    pub async fn list_workloads(
        &self,
        caller: &Caller,
        q: &ListWorkloads,
        page: Page,
    ) -> Result<Paged<WorkloadView>, AppError> {
        let within = match (q.project, q.organization) {
            (Some(p), _) => ObjectRef::new(Kind::Project, p),
            (None, Some(o)) => ObjectRef::new(Kind::Organization, o),
            (None, None) => {
                return Err(AppError::invalid("project", "a project or an organization"));
            }
        };
        self.authorize(caller, "workload.read", within).await?;
        let filter = WorkloadFilter { project: q.project, organization: q.organization };
        let read = self.store.workloads(&filter, page).await?;
        let next = page.next(&read, |w| w.id);
        let mut items = Vec::new();
        for w in read.into_iter().filter(|w| q.selector.matches(w.labels.as_map())) {
            items.push(self.view(w).await?);
        }
        Ok(Paged { items, next })
    }

    /// A workload's intents, oldest first.
    pub async fn list_intents(
        &self,
        caller: &Caller,
        workload: Uuid,
        page: Page,
    ) -> Result<Paged<Intent>, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, workload)).await?;
        let items = self.store.intents(workload, page).await?;
        let next = page.next(&items, |i| i.id);
        Ok(Paged { items, next })
    }
}
