// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placing a workload by hand (§2.3 of docs/05-scheduling.md): an operator binds a workload
//! whose approved target has no node to a node of their choosing, with their reason. The choice
//! overrides the node's cordon and taints, which are the operator's own decisions about it, and
//! nothing else: a node that cannot run the workload, or that its tenant's declaration excludes,
//! is refused whoever chooses it. A workload already on a node moves only by migration.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, PlacementWrite, RecipeSource, Store, WorkloadWrite};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::node::Node;
use korpis_core::placement::{BoundBy, Placement};
use korpis_core::schedule::{Candidate, Explanation, Rejection, Request, operator_refusals, place};
use korpis_core::tenancy::quota::Bounds;
use korpis_core::workload::{Intent, Workload};
use uuid::Uuid;

const OPERATION: &str = "workload.place_workload";

/// The longest reason, in characters.
const MAX_REASON: usize = 500;

/// A placement by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceWorkload {
    /// The workload.
    pub workload: Uuid,
    /// The node the operator chose.
    pub node: Uuid,
    /// Why, for the record.
    pub reason: String,
}

fn refused(rejections: &[Rejection]) -> AppError {
    let checks: Vec<String> =
        rejections.iter().map(|r| format!("{}: {}", r.check.as_str(), r.detail)).collect();
    AppError::Unsatisfiable {
        reason: "placement.unsatisfiable",
        message: format!("the node cannot run it: {}", checks.join("; ")),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Places a workload on the node an operator chose.
    pub async fn place_workload(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Placement>,
        cmd: PlaceWorkload,
    ) -> Result<Reply<Placement>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let reason = cmd.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON {
            return Err(AppError::invalid("reason", "why, in at most 500 characters"));
        }
        let permit =
            self.authorize(caller, "node.place", ObjectRef::new(Kind::Node, cmd.node)).await?;
        let object = ObjectRef::new(Kind::Workload, cmd.workload);
        self.authorize(caller, "workload.write", object).await?;
        let node = self.store.node(cmd.node).await?.ok_or(AppError::NotFound { kind: "node" })?;
        let mut tx = self.store.begin().await?;
        // The workload first, then the placement lock, as every placement takes them: in the
        // other order this deadlocked with the retry of an unplaced workload.
        let w = tx
            .lock_workload(cmd.workload)
            .await?
            .filter(|w| w.deleted_at.is_none())
            .ok_or(AppError::NotFound { kind: "workload" })?;
        tx.lock_placement().await?;
        let target = self.approved_target(&w).await?;
        match tx.placement_in(w.id).await? {
            Some(p) if p.node == node.id => return Ok(Reply::Fresh(p)),
            Some(_) => {
                return Err(AppError::NotYet { what: "moving a placed workload", phase: 3 });
            }
            None => {}
        }
        let (explanation, given) = self.choose(&mut tx, &w, &target, &node).await?;
        let placement = Placement {
            workload: w.id,
            node: node.id,
            intent: target.id,
            reserved: given.reserved,
            bound_at: self.clock.now(),
            bound_by: BoundBy::Operator,
            reason: reason.to_owned(),
            explanation,
            authoritative: true,
            held_at: None,
            devices: given.devices,
            cores: given.cores,
        };
        tx.bind(&placement).await?;
        tx.clear_unplaced(w.id).await?;
        self.count_placed(&mut tx, &w).await?;
        let mut e = caller.effect("workload.place", Outcome::Succeeded).under(permit.grant).after(
            details([
                ("node", node.id.to_string().into()),
                ("intent", target.id.to_string().into()),
                ("bound_by", "operator".into()),
                ("reason", reason.into()),
            ]),
        );
        e.workload = Some(w.id);
        e.node = Some(node.id);
        tx.append_effect(&e).await?;
        finish(tx, caller, idem, OPERATION, placement).await
    }

    /// The intent the workload was approved to be, which is what is placed.
    async fn approved_target(&self, w: &Workload) -> Result<Intent, AppError> {
        let id = w.target.ok_or_else(|| {
            AppError::invalid("workload", "nothing is approved to place; approve a plan first")
        })?;
        self.store.intent(id).await?.ok_or_else(|| AppError::Internal(format!("intent {id}")))
    }

    /// The explanation of an operator's choice of `node`: the scheduler's view of it, with
    /// the refusals the choice overrides; or why the node cannot run the workload at all.
    async fn choose(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        target: &Intent,
        node: &Node,
    ) -> Result<(Explanation, super::place::Given), AppError> {
        let owners = self.owners(w.organization).await?;
        let allocations = tx.allocations(&[node.id]).await?;
        let held = tx.device_holds(&[node.id], w.id).await?;
        let held_cores = tx.core_holds(&[node.id], w.id).await?;
        let peers = tx.peers(w.organization, w.id).await?;
        let empty = Bounds::default();
        let c = Candidate { node, allocated: allocations.get(&node.id).unwrap_or(&empty) };
        let volumes = self.existing_nodes(&target.body).await?;
        let (needed, mesh) = (self.needed(w, &target.body).await?, self.judged().await?.pairs());
        let drivers_refused = tx.refusals(w.id, target.id).await?;
        let learned = self.learned(tx, &[node.id], target).await?;
        let (entries, placed) = self.placed_entries(tx, w).await?;
        let promised = self.placed_promises(tx, w, (&entries, placed.as_ref())).await?;
        let quota = super::placed_quota::counting(&entries, placed.as_ref(), promised);
        let one = std::slice::from_ref(node);
        let addressing = self.addressing(tx, (w.id, &target.body), &owners, one).await?;
        let req = Request {
            workload: w.id,
            project: w.project,
            labels: w.labels.as_map(),
            owners: &owners,
            body: &target.body,
            current: None,
            edge_tier: addressing.stable(),
            routed: addressing.routed,
            edge_members: addressing.edge_members(),
            volumes: &volumes,
            refused: &drivers_refused,
            content: &learned.content,
            churn: &learned.churn,
            dependencies: &needed,
            mesh: &mesh,
            quota: &quota,
            devices: &held,
            cores: &held_cores,
            dedicated: &addressing.dedicated,
        };
        let policies = self.policies(&owners).await?;
        let standing = operator_refusals(&req, &c, &peers, &policies);
        if !standing.is_empty() {
            return Err(refused(&standing));
        }
        let mut explanation = place(&req, &[c], &peers, &policies);
        explanation.chosen = Some(node.id);
        let given = super::place::given_on(&req, one, node.id)?;
        let at = (owners.as_slice(), node);
        self.hold_addressing(tx, (w.id, &target.body), at, &addressing).await?;
        Ok((explanation, given))
    }
}
