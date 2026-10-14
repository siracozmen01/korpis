// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's own plan steps (§4.9 of docs/02-architecture.md, §6 of docs/03-state.md).
//! A step whose turn has come is queued as a job keyed by its plan and index, so it is queued
//! once however many replicas notice; a worker of any replica claims it, does it, and records its
//! completion as a node's is recorded, in the transaction that ends the job, so the step is
//! never completed without its job ending or the other way round. An attempt that fails is tried
//! again later, and the step fails when the job's attempts are spent.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite as _, Job, JobEnd, JobWrite as _, NewJob, RecipeSource, Store, Tx as _,
    WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::Plan;
use korpis_core::workload::plan::{PlanStatus, Step, StepExecutor};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// The kind of job a control-plane step is.
pub const PLAN_STEP: &str = "plan_step";

/// How many attempts a step is given before it fails.
const ATTEMPTS: u32 = 8;

/// How long after a failed attempt the next is made: doubling from half a minute, at most half
/// an hour.
#[must_use]
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(30u64.saturating_mul(1 << attempt.min(6))).min(Duration::from_secs(1800))
}

/// The step of `plan` the control plane executes next, if it is due at `now`: the plan is
/// approved or applying, its schedule has come, and its next step is the control plane's.
#[must_use]
pub fn due(plan: &Plan, now: SystemTime) -> Option<&Step> {
    if !matches!(plan.status, PlanStatus::Approved | PlanStatus::Applying)
        || plan.apply_after.is_some_and(|at| at > now)
    {
        return None;
    }
    let next = plan.steps.iter().find(|s| plan.completed.is_none_or(|c| s.index > c))?;
    (next.executor == StepExecutor::ControlPlane).then_some(next)
}

/// Which plan step a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepJob {
    /// The plan.
    pub plan: Uuid,
    /// The step's index.
    pub step: u32,
}

impl StepJob {
    /// The plan step `job` is, if it is one.
    #[must_use]
    pub fn of(job: &Job) -> Option<Self> {
        let plan = job.payload.get("plan")?.as_str()?.parse().ok()?;
        let step = u32::try_from(job.payload.get("step")?.as_u64()?).ok()?;
        Some(Self { plan, step })
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Queues every control-plane step that is due; how many were newly queued.
    pub async fn queue_control_steps(&self) -> Result<usize, AppError> {
        let now = self.clock.now();
        let plans = self.store.control_plane_plans().await?;
        let mut queued = 0;
        let mut tx = self.store.begin().await?;
        for (p, s) in plans.iter().filter_map(|p| due(p, now).map(|s| (p, s))) {
            let key = format!("plan:{}:{}", p.id, s.index);
            let job = NewJob {
                key: &key,
                kind: PLAN_STEP,
                payload: serde_json::json!({ "plan": p.id.to_string(), "step": s.index }),
                now,
                due: now,
                max_attempts: ATTEMPTS,
            };
            queued += usize::from(tx.enqueue_job(&job).await?);
        }
        tx.commit().await?;
        Ok(queued)
    }

    /// The plan and step of job `job`, if the step is still to be done: its plan is open and
    /// has not completed it. A cancelled plan begins no step (§7 of docs/03-state.md).
    pub async fn step_to_do(&self, job: &Job) -> Result<Option<(Plan, Step)>, AppError> {
        let Some(j) = StepJob::of(job) else { return Ok(None) };
        let Some(plan) = self.store.plan(j.plan).await? else { return Ok(None) };
        let open = matches!(plan.status, PlanStatus::Approved | PlanStatus::Applying);
        if !open || plan.completed.is_some_and(|c| c >= j.step) {
            return Ok(None);
        }
        let step = plan.steps.iter().find(|s| s.index == j.step).cloned();
        Ok(step.filter(|s| s.executor == StepExecutor::ControlPlane).map(|s| (plan, s)))
    }

    /// Ends job `job`, of `worker`, with nothing to do.
    pub async fn end_job_idle(&self, job: &Job, worker: &str) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.end_job((job.id, worker), &JobEnd::Done, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records step `step` of plan `plan` as done with `outcome`, leaving `after`, and ends job
    /// `job` of `worker`, in `tx`; an error if the worker no longer holds the job, so that
    /// nothing in `tx` is kept.
    pub(crate) async fn complete_control_step(
        &self,
        tx: &mut S::Tx,
        (job, worker): (&Job, &str),
        (plan, step): (Uuid, &Step),
        (outcome, after): (Outcome, serde_json::Map<String, serde_json::Value>),
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let end = match outcome {
            Outcome::Succeeded | Outcome::Skipped => JobEnd::Done,
            _ => JobEnd::Failed(after.get("reason").map(ToString::to_string).unwrap_or_default()),
        };
        if !tx.end_job((job.id, worker), &end, now).await? {
            return Err(AppError::conflict("job.lost", "another worker holds the job now"));
        }
        let locked = tx.lock_plan(plan).await?;
        let Some(locked) = locked else { return Err(AppError::NotFound { kind: "plan" }) };
        let mut e = Effect::new(
            Principal::System(SystemActor::Reconciler),
            format!("step.{}", step.action.as_str()),
            outcome,
        );
        e.workload = Some(locked.workload);
        e.plan = Some(plan);
        e.step = Some(step.index);
        e.grant = Some(locked.proposed_under);
        e = if outcome == Outcome::Failed { e.error(after) } else { e.after(after) };
        tx.append_effect(&e).await?;
        self.progress(tx, locked, step.index, outcome).await
    }

    /// After attempt `job` of `worker` failed with `error`: tried again later, or, with its
    /// attempts spent or when `again` is false, its step failed, which fails the plan.
    pub async fn control_step_failed(
        &self,
        (job, worker): (&Job, &str),
        error: &AppError,
        again: bool,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        if again && job.attempt < job.max_attempts {
            let end = JobEnd::Retry { at: now + backoff(job.attempt), error: error.to_string() };
            tx.end_job((job.id, worker), &end, now).await?;
        } else if let Some((plan, step)) = self.step_to_do(job).await? {
            let why =
                details([("reason", "step.failed".into()), ("message", error.to_string().into())]);
            self.complete_control_step(
                &mut tx,
                (job, worker),
                (plan.id, &step),
                (Outcome::Failed, why),
            )
            .await?;
        } else {
            tx.end_job((job.id, worker), &JobEnd::Failed(error.to_string()), now).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_back_off_doubling_to_half_an_hour() {
        let waits: Vec<u64> = (1..=8).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(waits, [60, 120, 240, 480, 960, 1800, 1800, 1800]);
    }
}
