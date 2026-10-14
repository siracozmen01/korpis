// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ingesting a node's journal (§4.10 of docs/02-architecture.md): exactly once, in order, each
//! record in the transaction that advances the node's cursor. A record is believed only as far
//! as the node's own assignments reach: an effect for a workload the node was never told about,
//! or for a step of a plan it does not execute, is kept as a rejection, visibly, and moves
//! nothing. The actor of a node-originated effect is the control plane executing a decision
//! already made, and its grant is the one the plan was approved under (§3.2 of
//! docs/01-model.md).

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite, JournalWrite, LeaseWrite, PlacementWrite, RecipeSource, RetirementWrite,
    Store, StreamWrite as _, Tx, WorkloadWrite,
};
use korpis_core::audit::{Effect, JournalOrigin, Outcome, details};
use korpis_core::authz::{Action, Kind, ObjectRef};
use korpis_core::node::journal::{Body, NodeEffect, Record, admit, valid_action};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::Plan;
use korpis_core::workload::assignment::retires;
use korpis_core::workload::plan::{PlanStatus, StepExecutor};
use std::time::SystemTime;
use uuid::Uuid;

/// The action a node reports console input under.
const CONSOLE_WRITE: &str = "workload.console.write";

fn system(action: &str, outcome: Outcome) -> Effect {
    Effect::new(Principal::System(SystemActor::Reconciler), action, outcome)
}

/// Where a record came from, set on every effect it produces.
fn origin(node: Uuid, r: &Record) -> impl Fn(Effect) -> Effect {
    let (seq, occurred_at, count) = (r.seq, r.occurred_at, r.count.max(1));
    move |mut e| {
        e.node = Some(node);
        e.origin = Some(JournalOrigin { node, seq });
        e.occurred_at = occurred_at;
        e.count = count;
        e
    }
}

fn rejected(why: &str, action: &str) -> Effect {
    system("journal.rejected", Outcome::Failed)
        .error(details([("reason", why.into()), ("action", action.into())]))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The highest journal sequence ingested from `node`: what it may discard up to.
    pub async fn journal_ingested(&self, node: Uuid) -> Result<u64, AppError> {
        Ok(self.store.journal_ingested(node).await?)
    }

    /// Ingests what of `records` follows the cursor without a gap; the cursor after.
    pub async fn ingest_journal(&self, node: Uuid, records: Vec<Record>) -> Result<u64, AppError> {
        let mut tx = self.store.begin().await?;
        let cursor = tx.lock_journal(node).await?;
        let admitted = admit(cursor, records);
        let Some(through) = admitted.last().map(|r| r.seq) else { return Ok(cursor) };
        for record in &admitted {
            let effect = self.apply(&mut tx, node, record).await?;
            tx.append_effect(&origin(node, record)(effect)).await?;
        }
        tx.advance_journal(node, through).await?;
        tx.commit().await?;
        Ok(through)
    }

    /// Applies one record; the effect that records it.
    async fn apply(&self, tx: &mut S::Tx, node: Uuid, r: &Record) -> Result<Effect, AppError> {
        Ok(match &r.body {
            Body::Effect(e) if e.action == CONSOLE_WRITE => {
                let at = r.occurred_at.unwrap_or_else(|| self.clock.now());
                self.console_input(node, e, at).await?
            }
            Body::Effect(e) => self.node_effect(tx, node, e).await?,
            Body::LeaseReleased { workload, epoch } => {
                let released = tx.release_lease(*workload, node, *epoch, self.clock.now()).await?;
                let mut e = system("lease.release", Outcome::Succeeded)
                    .after(details([("epoch", (*epoch).into()), ("current", released.into())]));
                e.workload = Some(*workload);
                e.grant = tx.decision_grant(*workload).await?;
                e
            }
            Body::BreakGlass { operator, reason, session } => {
                system("node.break_glass", Outcome::Succeeded).after(details([
                    ("operator", operator.clone().into()),
                    ("reason", reason.clone().into()),
                    ("session", session.clone().into()),
                ]))
            }
            Body::Gap { first, last, cause } => {
                system("journal.gap", Outcome::Failed).error(details([
                    ("first", (*first).into()),
                    ("last", (*last).into()),
                    ("cause", cause.clone().into()),
                ]))
            }
            Body::Segment { workload, kind, event, segment } => {
                // A segment is sealed as its container goes and pruned long after, so the test
                // is that the node once ran the workload, not that it still does.
                if !self.store.ever_leased(*workload, node).await? {
                    return Ok(rejected(
                        "the node never held the workload's lease",
                        event.action(),
                    ));
                }
                tx.record_segment((*workload, *kind), *event, segment, node).await?;
                let mut e = system(event.action(), Outcome::Succeeded).after(details([
                    ("stream", kind.as_str().into()),
                    ("first_offset", segment.first_offset.into()),
                    ("end_offset", segment.end_offset.into()),
                    ("bytes", segment.bytes.into()),
                    ("digest", segment.digest.clone().into()),
                ]));
                e.workload = Some(*workload);
                e.grant = self.store.governing_grant(*workload).await?;
                e
            }
            Body::Malformed { reason } => rejected(reason, "malformed"),
            // Metering is stored with the metering of Phase 2 (§4 of docs/20-roadmap.md); until
            // then its arrival is recorded, so the loss is visible rather than silent.
            Body::Metering => system("journal.unsupported", Outcome::Skipped)
                .after(details([("kind", "metering".into())])),
        })
    }

    /// Console input the node carried, recorded as the act of the subject it names, if that
    /// subject could have given it (§6 of docs/14-streams.md): the node held the workload's
    /// lease, and the grant named is the subject's and permitted the input when it happened.
    async fn console_input(
        &self,
        node: Uuid,
        n: &NodeEffect,
        at: SystemTime,
    ) -> Result<Effect, AppError> {
        let (Some(w), Some(actor), Some(grant)) = (n.workload, &n.actor, n.grant) else {
            let why = "console input names its workload, who sent it, and their grant";
            return Ok(rejected(why, &n.action));
        };
        if !self.store.ever_leased(w, node).await? {
            return Ok(rejected("the node never held the workload's lease", &n.action));
        }
        let action = Action::parse(CONSOLE_WRITE).map_err(|e| AppError::Internal(e.to_string()))?;
        let on = (ObjectRef::new(Kind::Workload, w), grant);
        if !self.attest(actor, &action, on, at).await? {
            let why = "the grant named did not permit its subject console input then";
            return Ok(rejected(why, &n.action));
        }
        let mut e = Effect::new(actor.clone(), &n.action, n.outcome);
        e.workload = Some(w);
        e.grant = Some(grant);
        e.after.clone_from(&n.after);
        e.error.clone_from(&n.error);
        Ok(e)
    }

    /// An effect the node reports, checked against what the node was told.
    async fn node_effect(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        n: &NodeEffect,
    ) -> Result<Effect, AppError> {
        if !valid_action(&n.action) {
            return Ok(rejected("the action is not a valid name", "invalid"));
        }
        if let Some(w) = n.workload
            && !self.store.nodes_of(w).await?.contains(&node)
        {
            return Ok(rejected("the workload is not assigned to this node", &n.action));
        }
        let mut e = system(&n.action, n.outcome);
        e.workload = n.workload;
        e.error.clone_from(&n.error);
        e.after.clone_from(&n.after);
        let Some(plan_id) = n.plan else {
            // Outside a plan the node is keeping a decision already made (a crash restart, an
            // adoption), so the effect names the grant that decision was approved under.
            if let Some(w) = n.workload {
                e.grant = self.store.governing_grant(w).await?;
            }
            return Ok(e);
        };
        let plan = tx.lock_plan(plan_id).await?;
        let step = n.step.filter(|s| {
            plan.as_ref().is_some_and(|p| {
                Some(p.workload) == n.workload
                    && p.steps.iter().any(|x| x.index == *s && x.executor == StepExecutor::Node)
            })
        });
        let (Some(plan), Some(step)) = (plan, step) else {
            return Ok(rejected("not a step of a plan this node executes", &n.action));
        };
        e.plan = Some(plan.id);
        e.step = Some(step);
        e.grant = Some(plan.proposed_under);
        self.progress(tx, plan, step, n.outcome).await?;
        Ok(e)
    }

    /// Moves a plan by a step's outcome: its progress, `applied` after its last step, `failed`
    /// on a failure. A retire plan applied retires the workload.
    async fn progress(
        &self,
        tx: &mut S::Tx,
        mut plan: Plan,
        step: u32,
        outcome: Outcome,
    ) -> Result<(), AppError> {
        let from = plan.status;
        if from == PlanStatus::Cancelled {
            return self.after_cancel(tx, plan, step, outcome).await;
        }
        if !matches!(from, PlanStatus::Approved | PlanStatus::Queued | PlanStatus::Applying) {
            return Ok(());
        }
        let moved =
            |e: korpis_core::workload::plan::TransitionError| AppError::Internal(e.to_string());
        if from != PlanStatus::Applying {
            plan.transition(PlanStatus::Applying).map_err(moved)?;
        }
        match outcome {
            Outcome::Succeeded | Outcome::Skipped => {
                if plan.completed.is_some_and(|c| c >= step) {
                    return Ok(());
                }
                plan.completed = Some(step);
                if plan.steps.last().is_some_and(|s| s.index == step) {
                    plan.transition(PlanStatus::Applied).map_err(moved)?;
                }
            }
            Outcome::Failed | Outcome::RolledBack | Outcome::Denied => {
                plan.transition(PlanStatus::Failed).map_err(moved)?;
            }
        }
        if !tx.update_plan(&plan, from).await? {
            return Err(AppError::conflict("plan.moved", "the plan changed meanwhile"));
        }
        if plan.status == PlanStatus::Applied && retires(&plan) {
            self.finish_retirement(tx, &plan).await?;
        }
        Ok(())
    }

    /// A step that was in flight when its plan was cancelled finished (§7 of
    /// docs/03-state.md): it counts toward what the plan completed, and moves nothing else.
    async fn after_cancel(
        &self,
        tx: &mut S::Tx,
        mut plan: Plan,
        step: u32,
        outcome: Outcome,
    ) -> Result<(), AppError> {
        let done = matches!(outcome, Outcome::Succeeded | Outcome::Skipped);
        if !done || plan.completed.is_some_and(|c| c >= step) {
            return Ok(());
        }
        plan.completed = Some(step);
        if !tx.update_plan(&plan, PlanStatus::Cancelled).await? {
            return Err(AppError::conflict("plan.moved", "the plan changed meanwhile"));
        }
        Ok(())
    }

    /// The node removed the workload: its placement goes, and its identity is retired (§3.3 of
    /// docs/01-model.md, §4.8 of docs/02-architecture.md).
    async fn finish_retirement(&self, tx: &mut S::Tx, plan: &Plan) -> Result<(), AppError> {
        let w = tx
            .lock_workload(plan.workload)
            .await?
            .ok_or_else(|| AppError::Internal(format!("workload {}", plan.workload)))?;
        tx.retire_workload(w.id, self.clock.now()).await?;
        self.place_target(tx, &w, None).await?;
        let mut e = system("workload.retire", Outcome::Succeeded);
        e.workload = Some(w.id);
        e.plan = Some(plan.id);
        e.step = plan.completed;
        e.grant = Some(plan.proposed_under);
        tx.append_effect(&e).await?;
        Ok(())
    }
}
