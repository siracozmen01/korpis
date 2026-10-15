// SPDX-License-Identifier: AGPL-3.0-or-later

//! Executing a plan's node steps (§4.8 and §4.9 of docs/02-architecture.md): in order, the next
//! one not yet executed, never past `through`, and none once the plan is cancelled, which is read
//! again before every step. Each completion is journalled, keyed by plan and
//! step, before it is kept as done, and a failure is journalled and stops the plan here: the
//! control plane fails it, and the next assignment no longer carries it. A failure because the
//! driver refused, before anything of the workload was made here, is journalled as
//! `step.refused`, which lets the control plane place the workload elsewhere instead.

use super::act::Failure;
use super::progress::Step;
use super::{Reconciler, Tracked, effect, text};
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind;
use std::collections::BTreeMap;

/// What a step leaves behind, for its effect.
pub(super) type After = BTreeMap<String, pb::Value>;

pub(super) fn detail<'a>(s: &'a pb::Step, key: &str) -> Option<&'a str> {
    match s.detail.get(key)?.kind.as_ref()? {
        Kind::StringValue(v) => Some(v.as_str()),
        _ => None,
    }
}

pub(super) fn flag(s: &pb::Step, key: &str) -> bool {
    matches!(s.detail.get(key).and_then(|v| v.kind.as_ref()), Some(Kind::BoolValue(true)))
}

pub(super) fn number(s: &pb::Step, key: &str) -> u64 {
    match s.detail.get(key).and_then(|v| v.kind.as_ref()) {
        Some(Kind::IntValue(n)) => u64::try_from(*n).unwrap_or(0),
        _ => 0,
    }
}

/// A step's failure for `why`: `step.refused` when the driver refused before anything of the
/// workload was made here, which lets the control plane place it elsewhere (§2.1 of
/// docs/05-scheduling.md), and `step.failed` otherwise.
fn failure(why: String, refused_before_anything: bool) -> pb::Error {
    let reason = if refused_before_anything { "step.refused" } else { "step.failed" };
    pb::Error { reason: reason.into(), message: why, ..Default::default() }
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Executes what is outstanding of `plan`; whether nothing is left that the node may do.
    pub(super) async fn steps(
        &mut self,
        a: &agent::Assignment,
        plan: &agent::NodePlan,
        t: &mut Tracked,
    ) -> bool {
        let done = self.progress.of(&a.workload).step.filter(|s| s.plan == plan.plan);
        if done.as_ref().is_some_and(|s| s.failed) {
            return false;
        }
        let after = done.map(|s| s.index);
        // Whether anything of the workload was made here: a step of this plan completed, or a
        // runtime object exists. A refusal before then leaves nothing to move.
        let mut made = after.is_some() || t.runtime.is_some();
        let outstanding: Vec<&pb::Step> = plan
            .steps
            .iter()
            .filter(|s| after.is_none_or(|i| s.index > i) && s.index <= plan.through)
            .collect();
        for s in outstanding {
            if !self.still_assigned(&a.workload, &plan.plan, s.index) {
                // Cancelled since this pass began: the step before finished, and this one is
                // not begun (§7 of docs/03-state.md). The next pass acts on the new assignment.
                return false;
            }
            let result = match self.execute(a, &plan.plan, s, t).await {
                Ok(None) => return false,
                Ok(Some(left)) => Ok(left),
                Err(e) => Err(e),
            };
            let ok = result.is_ok();
            let mut e = effect(Some(&a.workload), &format!("step.{}", s.action), ok);
            e.plan = Some(plan.plan.clone());
            e.step = Some(s.index);
            match result {
                Ok(left) => e.after = left.into_iter().collect(),
                Err(Failure(why)) => e.error = failure(why, t.rejected.is_some() && !made).into(),
            }
            made |= ok;
            if !self.record(e).await {
                return false;
            }
            let kept = Step { plan: plan.plan.clone(), index: s.index, failed: !ok };
            if let Err(e) = self.progress.stepped(&a.workload, kept) {
                tracing::error!(error = %e, "progress could not be kept; the step runs again");
                return false;
            }
            if !ok {
                return false;
            }
        }
        plan.steps.iter().all(|s| s.index <= plan.through)
    }

    /// Whether the set held now still lets this node execute step `index` of `plan` for
    /// `workload`. A cancelled plan leaves the assignment, and a lowered `through` stops short
    /// of the step; either is read between steps, never only when a pass begins.
    fn still_assigned(&self, workload: &str, plan: &str, index: u32) -> bool {
        let held = self.wiring.held.borrow();
        held.assignments
            .get(workload)
            .and_then(|a| a.plan.as_option())
            .is_some_and(|p| p.plan == plan && index <= p.through)
    }

    /// Executes one step: what it leaves, or `None` for a step that has not ended yet, which a
    /// later pass moves on.
    async fn execute(
        &mut self,
        a: &agent::Assignment,
        plan: &str,
        s: &pb::Step,
        t: &mut Tracked,
    ) -> Result<Option<After>, Failure> {
        let mut after = After::new();
        match s.action.as_str() {
            "backup_volume" => return self.backup_step(a, plan, s).await,
            "restore_volume" => return self.restore_step(a, plan, s).await,
            "replace_volume" => return self.replace_step(s).await.map(Some),
            "install" => return self.install_step(a, plan, s, t).await,
            "pull_recipe" => {
                let spec = self.prepare(a, t).await?;
                self.host.fetch(&spec.rootfs).await?;
                after.insert("rootfs".into(), text(&spec.rootfs));
            }
            "create_volume" | "attach_volume" | "detach_volume" | "delete_volume"
            | "snapshot_volume" | "rollback_volume" | "delete_snapshot" => {
                after.extend(self.volume_step(a, s).await?);
            }
            "create" => {
                let id = self.current(a, t).await?;
                after.insert("runtime_id".into(), text(&id));
            }
            "start" => {
                let id = self.start(a, t).await?;
                after.insert("runtime_id".into(), text(&id));
            }
            "stop" => self.stop(t).await?,
            "restart" => {
                let id = self.restart(a, t).await?;
                after.insert("runtime_id".into(), text(&id));
            }
            // Validated now, applied at the next start: the object is rebuilt from the target.
            "reconfigure" => {
                self.prepare(a, t).await?;
                after.insert("applies_at".into(), text("the next start"));
            }
            "reconfigure_network" => {
                after.insert("applies_at".into(), text("the next start"));
            }
            "retire" => self.destroy(t).await?,
            other => return Err(Failure(format!("a node does not execute {other:?}"))),
        }
        Ok(Some(after))
    }
}
