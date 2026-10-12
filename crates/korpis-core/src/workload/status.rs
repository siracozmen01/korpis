// SPDX-License-Identifier: AGPL-3.0-or-later

//! How far a workload is from what was approved for it (§4.3 of docs/02-architecture.md).
//!
//! Two states are facts about plans, held by the control plane and never inferred from a node:
//! a plan waiting for approval, and an approved plan waiting for its window. The others are
//! measured from what the node reports; with no report there is no measurement, and the answer
//! is `Unknown` (or `Unplaced`, when no node was chosen), never a remembered value (P4).

use super::plan::PlanStatus;
use super::{Plan, PlanTarget, Workload};
use std::time::SystemTime;

/// A workload's convergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Convergence {
    /// Everything reported matches the assignment.
    Converged,
    /// A plan is being applied, or the node has not caught up.
    Applying,
    /// A plan waits for approval or confirmation.
    AwaitingApproval,
    /// An approved plan waits for its window.
    Scheduled,
    /// The node holds the assignment and cannot satisfy it.
    Failing,
    /// No observation within the staleness window.
    Unknown,
    /// No node has been chosen.
    Unplaced,
}

/// The plan a status shows: the one applying, else one waiting for a human, else an approved
/// one; the newest of its kind.
#[must_use]
pub fn active_plan(plans: &[Plan]) -> Option<&Plan> {
    let rank = |p: &Plan| match p.status {
        PlanStatus::Applying => Some(0),
        PlanStatus::Proposed => Some(1),
        PlanStatus::Approved | PlanStatus::Queued => Some(2),
        _ => None,
    };
    plans
        .iter()
        .filter_map(|p| rank(p).map(|r| (r, p)))
        .min_by(|(ra, a), (rb, b)| ra.cmp(rb).then(b.computed_at.cmp(&a.computed_at)))
        .map(|(_, p)| p)
}

/// Whether `plan` is for the workload's newest declaration, of intent or of power.
fn for_head(plan: &Plan, w: &Workload) -> bool {
    match plan.target {
        PlanTarget::Intent(i) => i == w.intent,
        PlanTarget::Power(v) => v == w.power_version,
        PlanTarget::Retire | PlanTarget::Migrate => true,
    }
}

/// The convergence the control plane can state without a node, from the workload's open plans
/// and whether it is placed. A declaration ahead of what was approved, with a plan proposed for
/// it, is awaiting approval; an approved plan with a window still ahead is scheduled.
/// Observations refine `Unknown` once nodes report them.
#[must_use]
pub fn convergence(w: &Workload, open: &[Plan], placed: bool, now: SystemTime) -> Convergence {
    let has = |f: &dyn Fn(&Plan) -> bool| open.iter().any(f);
    if has(&|p| p.status == PlanStatus::Proposed && for_head(p, w)) {
        return Convergence::AwaitingApproval;
    }
    let waiting = |p: &Plan| {
        matches!(p.status, PlanStatus::Approved | PlanStatus::Queued)
            && p.apply_after.is_some_and(|after| after > now)
    };
    if has(&waiting) {
        return Convergence::Scheduled;
    }
    if !placed {
        return Convergence::Unplaced;
    }
    if has(&|p| p.status == PlanStatus::Applying) {
        return Convergence::Applying;
    }
    Convergence::Unknown
}
