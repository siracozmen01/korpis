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
use crate::ports::{Clock, RecipeSource, ReplicaWrite as _, Store, WorkloadWrite as _};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::plan::compute::{HeldVolume, Promotion, remake_steps};
use korpis_core::workload::plan::impact::assess;
use korpis_core::workload::plan::{Approval, PROPOSAL_LIFETIME, PlanStatus, ResourceDelta};
use korpis_core::workload::spec::ReplicaFailover;
use korpis_core::workload::volume::{Home, Volume};
use korpis_core::workload::{Intent, IntentBody, Plan, PlanTarget, PowerState, Workload};
use std::time::SystemTime;
use uuid::Uuid;

/// A volume a workload made elsewhere holds.
#[derive(Debug, Clone)]
pub(super) struct Held {
    /// The name it mounts it under.
    pub(super) name: String,
    /// Its record.
    pub(super) id: Uuid,
    /// Where it is: for one made from a replica, the replica's node.
    pub(super) home: Home,
    /// The replica it is made from, where its own node is gone.
    pub(super) promote: Option<Promoted>,
}

/// A volume made from its freshest replica, its own node gone (§2.2 of docs/06-storage.md).
#[derive(Debug, Clone)]
pub(super) struct Promoted {
    /// The replica's last snapshot, and the class it is kept in.
    pub(super) promotion: Promotion,
    /// The node the volume was on, which keeps what the replica lacks.
    pub(super) from: Uuid,
    /// When the replica received that snapshot: what was written since is lost.
    pub(super) received_at: SystemTime,
}

/// What a volume `v` on `gone`, of a workload whose declaration is `body`, is made from: its
/// freshest replica, where the declaration accepts the loss and one holds a snapshot.
fn promoted(v: &Volume, body: &IntentBody, gone: Option<Uuid>) -> Option<Promoted> {
    let from = gone.filter(|g| v.home() == Home::Node(*g))?;
    if body.replica_failover != ReplicaFailover::Automatic {
        return None;
    }
    let (snapshot, received_at) =
        v.replicas.iter().filter_map(|r| r.held).max_by_key(|(_, at)| *at)?;
    let storage_class = v.storage_class.clone();
    let into = format!("{}.{}", Id::from_uuid(IdKind::Volume, v.id), snapshot.simple());
    let promotion = Promotion { snapshot, storage_class, into };
    Some(Promoted { promotion, from, received_at })
}

/// The node keeping the replica `p` is made from, of `v`.
fn replica_node(v: &Volume, p: &Promoted) -> Option<Uuid> {
    let snapshot = p.promotion.snapshot;
    v.replicas.iter().find(|r| r.held.is_some_and(|(s, _)| s == snapshot)).map(|r| r.node)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Each volume `body` declares for `w` that `w` holds, made with it or attached, with where
    /// it is. A volume made with it that has no record was never reported made, and is made
    /// again where it lands: its node reports a volume it makes before the step that made it
    /// completes, so a plan that applied left a record of each. One on `gone`, the node a
    /// failover leaves, is reached at its freshest replica where the declaration accepts that
    /// replica's loss (§2.2 of docs/06-storage.md).
    pub(super) async fn held_volumes(
        &self,
        w: &Workload,
        (body, gone): (&IntentBody, Option<Uuid>),
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
                let promote = promoted(v, body, gone);
                let home = match promote.as_ref().and_then(|p| replica_node(v, p)) {
                    Some(node) => Home::Node(node),
                    None => v.home(),
                };
                held.push(Held { name: spec.name.clone(), id: v.id, home, promote });
            }
        }
        Ok(held)
    }

    /// Writes the remake plan that makes `w`'s `target` on `node`, the one it was just bound to,
    /// with the volumes it holds, approved by the control plane under `grant`; its identifier.
    /// A volume made from its replica moves to `node` here, in the same transaction.
    pub(super) async fn remake(
        &self,
        tx: &mut S::Tx,
        (w, target): (&Workload, &Intent),
        (held, node): (&[Held], Uuid),
        grant: Option<Uuid>,
    ) -> Result<Uuid, AppError> {
        let now = self.clock.now();
        for (h, p) in held.iter().filter_map(|h| h.promote.as_ref().map(|p| (h, p))) {
            let made = (p.promotion.into.as_str(), p.promotion.snapshot, p.received_at);
            if !tx.promote_replica((h.id, p.from, node), made, now).await? {
                let why = format!("volume {} left node {} meanwhile", h.id, p.from);
                return Err(AppError::conflict("volume.moved", why));
            }
        }
        let power = match w.power_target_version {
            0 => PowerState::Stopped,
            v => {
                self.store.power_at(w.id, v).await?.map_or(PowerState::Stopped, |p| p.power.desired)
            }
        };
        let (driver, disruption, wait) = self.running_layers(w).await?;
        let classifier = propose::classifier(&driver, (&disruption, wait), w);
        let held: Vec<HeldVolume> = held
            .iter()
            .map(|h| HeldVolume {
                name: h.name.clone(),
                record: h.id,
                promote: h.promote.as_ref().map(|p| p.promotion.clone()),
            })
            .collect();
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
