// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading workloads (§4.3 of docs/02-architecture.md): everything a client needs to render one
//! honestly, and the intent chain. Existence is gated by read authority (§8 of docs/10-api.md).

use super::explain::{Found, PlacementView, UnplacedView};
use super::observed::Observed;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store, WorkloadFilter};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::labels::LabelSelector;
use korpis_core::tenancy::Name;
use korpis_core::workload::set::drifted;
use korpis_core::workload::status::{Convergence, active_plan};
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
    /// Where it is placed, as the caller may see it.
    pub placement: Option<PlacementView>,
    /// Why its target has no node, if the newest attempt found none.
    pub unplaced: Option<UnplacedView>,
    /// What its node last reported, and whether that is current.
    pub observation: Option<Observed>,
    /// Present when it belongs to a set and its head was declared outside it.
    pub drift: Option<Drift>,
}

/// A member of a set whose head was declared outside it (§6 of docs/13-surface-cli.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    /// The set.
    pub set: Uuid,
    /// The newest intent the set declared, which its files still say.
    pub set_intent: Option<Uuid>,
    /// The effect that recorded declaring the head outside the set.
    pub effect: Option<Uuid>,
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
    /// Members of the declared set of this name; needs `project`.
    pub set: Option<Name>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    async fn unrendered(&self, workload: Workload) -> Result<(WorkloadView, Found), AppError> {
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
        let found = self.found(&workload, target.as_ref()).await?;
        let placed = found.placed_for(workload.target);
        let (convergence, observation) =
            self.measure(&workload, &open, found.node(), placed).await?;
        let drift = match workload.set.filter(|_| drifted(&workload, &head)) {
            Some(set) => Some(Drift {
                set,
                set_intent: self.store.set_intent(id, set).await?,
                effect: self.store.declaring_effect(id, head.id).await?,
            }),
            None => None,
        };
        let view = WorkloadView {
            drift,
            workload,
            power,
            head,
            target,
            active_plan: active,
            convergence,
            placement: None,
            unplaced: None,
            observation,
        };
        Ok((view, found))
    }

    /// Views of `workloads`, their placements filtered for the caller.
    async fn views(
        &self,
        caller: &Caller,
        workloads: Vec<Workload>,
    ) -> Result<Vec<WorkloadView>, AppError> {
        let (mut views, mut found) = (Vec::new(), Vec::new());
        for w in workloads {
            let (v, f) = self.unrendered(w).await?;
            views.push(v);
            found.push(f);
        }
        self.render_placements(caller, &mut views, found).await?;
        Ok(views)
    }

    /// One workload the caller may read, deleted or not.
    pub async fn get_workload(&self, caller: &Caller, id: Uuid) -> Result<WorkloadView, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, id)).await?;
        let w = self.store.workload(id).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        let mut views = self.views(caller, vec![w]).await?;
        views.pop().ok_or_else(|| AppError::Internal(format!("workload {id} rendered nothing")))
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
        let set = match (&q.set, q.project) {
            (None, _) => None,
            (Some(_), None) => return Err(AppError::invalid("set", "a set is named in a project")),
            (Some(n), Some(p)) => match self.store.declared_set(p, n.as_str()).await? {
                Some(s) => Some(s.id),
                None => return Ok(Paged { items: Vec::new(), next: None }),
            },
        };
        let filter = WorkloadFilter { project: q.project, organization: q.organization, set };
        let read = self.store.workloads(&filter, page).await?;
        let next = page.next(&read, |w| w.id);
        let matching = read.into_iter().filter(|w| q.selector.matches(w.labels.as_map())).collect();
        Ok(Paged { items: self.views(caller, matching).await?, next })
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
