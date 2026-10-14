// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which job each step works through, and forgetting the jobs nothing names any more (§5.5 of
//! docs/06-storage.md). A job is named once for its plan, so the plan's steps and a pass after a
//! restart all find the same one, and it is kept for as long as its plan is in an assignment: a
//! step never forgets its own, because doing it again after a crash must find the job it began,
//! and a replacing restore's staging place outlives the step that made it. Once the plan has left
//! the set, the helper is asked to forget the job, which releases what it keeps on the volume. A
//! job still running is never touched, and nothing is forgotten before a set has been held.

use super::Reconciler;
use super::step::detail;
use crate::held::HeldSet;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::time::Instant;

/// How many of the reconciler's periods pass between two looks at the helper's jobs.
const REAP_EVERY: u32 = 30;

/// The job step `s` works through, if it is a job's step.
pub(super) fn job_of(s: &pb::Step) -> Option<String> {
    match s.action.as_str() {
        "backup_volume" => detail(s, "backup").map(|b| format!("bkp-{b}")),
        "restore_volume" | "replace_volume" => detail(s, "job").map(|j| format!("rst-{j}")),
        _ => None,
    }
}

/// Every job a step of a plan in `held` names.
fn named(held: &HeldSet) -> BTreeSet<String> {
    let steps = held.assignments.values().filter_map(|a| a.plan.as_option());
    steps.flat_map(|p| p.steps.iter().filter_map(job_of)).collect()
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Forgets every job that ended and that no plan in `held` names, at most once a while.
    pub(super) async fn reap(&mut self, held: &HeldSet) {
        let due = self.reaped.is_none_or(|at| at.elapsed() >= self.wiring.period * REAP_EVERY);
        if !due || held.digest.is_empty() {
            return;
        }
        self.reaped = Some(Instant::now());
        let jobs = match self.host.list_jobs().await {
            Ok(jobs) => jobs,
            Err(e) => {
                tracing::warn!(error = %e, "the helper's jobs could not be listed");
                return;
            }
        };
        let named = named(held);
        let ended = |j: &&helper::JobSummary| {
            j.state.as_known() != Some(helper::JobState::JOB_STATE_RUNNING)
        };
        for j in jobs.iter().filter(ended).filter(|j| !named.contains(&j.job)) {
            match self.host.forget_job(&j.job).await {
                Ok(()) => tracing::info!(job = %j.job, "a job no plan names was forgotten"),
                Err(e) => tracing::warn!(job = %j.job, error = %e, "a job could not be forgotten"),
            }
        }
    }
}
