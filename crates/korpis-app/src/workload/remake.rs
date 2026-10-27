// SPDX-License-Identifier: AGPL-3.0-or-later

//! Making a workload again on the node it was placed on again (§2.4 of docs/05-scheduling.md,
//! Finding 204 of docs/23-walkthroughs.md). What a node runs it from, its recipe's content and
//! its volumes there, was put on the node by the steps of the plan that made it, and those ran on
//! the node it left. So a workload made on one node and placed on another while nothing of it is
//! under way, by failover or away from a node removed as compromised, is placed only where every
//! volume it holds is reached, and the control plane approves a remake plan for it there: its
//! recipe pulled, each volume attached where it is, its runtime created and, if it is to run,
//! started. The plan carries out the decision that approved its target, under that grant.

use super::propose;
use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, RecipeSource, Store, WorkloadWrite as _};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::plan::compute::remake_steps;
use korpis_core::workload::plan::impact::assess;
use korpis_core::workload::plan::{Approval, PROPOSAL_LIFETIME, PlanStatus, ResourceDelta};
use korpis_core::workload::volume::{Home, Volume};
use korpis_core::workload::{Intent, IntentBody, Plan, PlanTarget, PowerState, Workload};
use uuid::Uuid;

/// A volume a workload made elsewhere holds.
#[derive(Debug, Clone)]
pub(super) struct Held {
    /// The name it mounts it under.
    pub(super) name: String,
    /// Its record.
    pub(super) id: Uuid,
    /// Where it is.
    pub(super) home: Home,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Each volume `body` declares for `w` that `w` holds, made with it or attached, with where
    /// it is. A volume made with it that has no record was never reported made, and is made
    /// again where it lands: its node reports a volume it makes before the step that made it
    /// completes, so a plan that applied left a record of each.
    pub(super) async fn held_volumes(
        &self,
        w: &Workload,
        body: &IntentBody,
    ) -> Result<Vec<Held>, AppError> {
        let mut records: Vec<Volume> = Vec::new();
        let mut page = Page::new(None, Page::DEFAULT);
        loop {
            let found = self.store.volumes(w.project, Some(w.id), false, page).await?;
            records.extend(found.iter().filter(|v| v.attached_to == Some(w.id)).cloned());
            match page.next(&found, |v| v.id) {
                Some(after) => page = Page::new(Some(after), page.size),
                None => break,
            }
        }
        let mut held = Vec::new();
        for spec in &body.volumes {
            let found = match spec.existing {
                Some(existing) => records.iter().find(|v| v.id == existing),
                None => records.iter().find(|v| v.name == spec.name),
            };
            if let Some(v) = found {
                held.push(Held { name: spec.name.clone(), id: v.id, home: v.home() });
            }
        }
        Ok(held)
    }

    /// Writes the remake plan that makes `w`'s `target` on the node it was just bound to, with
    /// the volumes it holds, approved by the control plane under `grant`; its identifier.
    pub(super) async fn remake(
        &self,
        tx: &mut S::Tx,
        (w, target): (&Workload, &Intent),
        held: &[Held],
        grant: Option<Uuid>,
    ) -> Result<Uuid, AppError> {
        let now = self.clock.now();
        let power = match w.power_target_version {
            0 => PowerState::Stopped,
            v => {
                self.store.power_at(w.id, v).await?.map_or(PowerState::Stopped, |p| p.power.desired)
            }
        };
        let (driver, disruption, wait) = self.running_layers(w).await?;
        let classifier = propose::classifier(&driver, (&disruption, wait), w);
        let held: Vec<(String, Uuid)> = held.iter().map(|h| (h.name.clone(), h.id)).collect();
        let steps = remake_steps(w.id, (&target.body, power), &held, classifier);
        let system = Principal::System(SystemActor::Scheduler);
        let plan = Plan {
            id: Uuid::now_v7(),
            workload: w.id,
            target: PlanTarget::Remake,
            operation: None,
            from_intent: w.target,
            from_power: w.power_target_version,
            computed_at: now,
            computed_by: system.clone(),
            proposed_under: grant,
            impact: assess(&steps, ResourceDelta::default()),
            steps,
            approval: Approval::Auto,
            approval_reason: "it carries out the decision that approved the workload's target, \
                              on the node it was placed on again"
                .to_owned(),
            status: PlanStatus::Approved,
            completed: None,
            apply_after: None,
            expires_at: now + PROPOSAL_LIFETIME,
            approved_by: Some(system),
            approved_at: Some(now),
        };
        tx.insert_plan(&plan).await?;
        Ok(plan.id)
    }
}
