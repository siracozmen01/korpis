// SPDX-License-Identifier: AGPL-3.0-or-later

//! How far a workload is from what was approved for it (§4.3 of docs/02-architecture.md).
//!
//! Two states are facts about plans, held by the control plane and never inferred from a node:
//! a plan waiting for approval, and an approved plan waiting for its window. The others are
//! measured from what the node reports; with no report there is no measurement, and the answer
//! is `Unknown` (or `Unplaced`, when no node was chosen), never a remembered value (P4).

use super::observation::{Observation, ObservedState};
use super::plan::PlanStatus;
use super::{Plan, PlanTarget, Power, PowerState, Workload};
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

impl Convergence {
    /// Every convergence.
    pub const ALL: [Self; 7] = [
        Self::Converged,
        Self::Applying,
        Self::AwaitingApproval,
        Self::Scheduled,
        Self::Failing,
        Self::Unknown,
        Self::Unplaced,
    ];

    /// Its name in events and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Converged => "converged",
            Self::Applying => "applying",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Scheduled => "scheduled",
            Self::Failing => "failing",
            Self::Unknown => "unknown",
            Self::Unplaced => "unplaced",
        }
    }

    /// The convergence a name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
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
        // Deleting a volume the workload no longer holds, or pruning its backups, asks nothing
        // of the workload itself.
        PlanTarget::Volume(..) | PlanTarget::Prune(_) => false,
    }
}

/// What a node reported about a workload, with what it is measured against.
#[derive(Debug, Clone, Copy)]
pub struct Reported<'a> {
    /// The latest observation.
    pub observation: &'a Observation,
    /// Whether the node is heard. An observation from a node that missed three heartbeats is
    /// not current, however recent it looks (question 6 of §10 of docs/02-architecture.md).
    pub fresh: bool,
    /// The generation of the node's assignment now.
    pub generation: u64,
    /// The approved power declaration.
    pub power: &'a Power,
}

/// How far a workload is from what was approved (§4.3 of docs/02-architecture.md). A
/// declaration ahead of what was approved, with a plan proposed for it, is awaiting approval; an
/// approved plan with a window still ahead is scheduled; an approved target with no node is
/// unplaced. Those are the control plane's facts. Everything else is measured from what the node
/// reported, and with no current report the answer is `Unknown`.
#[must_use]
pub fn convergence(
    w: &Workload,
    open: &[Plan],
    placed: bool,
    now: SystemTime,
    reported: Option<Reported<'_>>,
) -> Convergence {
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
    let Some(r) = reported.filter(|r| r.fresh) else { return Convergence::Unknown };
    let o = r.observation;
    if o.assignment_seen < r.generation || has(&|p| p.status == PlanStatus::Applying) {
        return Convergence::Applying;
    }
    let desired = match r.power.desired {
        PowerState::Running => ObservedState::Running,
        PowerState::Stopped => ObservedState::Stopped,
    };
    let at_target = o.intent_seen == w.target
        && o.power_seen == r.power.version
        && o.cycle_seen == r.power.cycle;
    match o.state {
        ObservedState::Unsatisfiable | ObservedState::Crashed => Convergence::Failing,
        state if at_target && state == desired => Convergence::Converged,
        ObservedState::Starting | ObservedState::Stopping => Convergence::Applying,
        // The node holds the assignment, nothing is outstanding, and it still disagrees.
        _ => Convergence::Failing,
    }
}
