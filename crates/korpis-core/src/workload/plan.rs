// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans: the computed difference between a declaration and reality, persisted, inspectable,
//! and approvable (§3.3 of docs/01-model.md). Every change to what a node is assigned passes
//! through one (P5).

pub mod approval;
pub mod compute;
pub mod impact;
pub mod status;
pub mod step;

pub use approval::{Approval, Proposing};
pub use impact::{Estimate, Impact, ResourceDelta};
pub use status::PlanStatus;
pub use step::{
    Classifier, DriverClass, Layer, Override, OverrideRefusal, Step, StepAction, StepExecutor,
};

use crate::principal::Principal;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long a proposed plan stays approvable. A stale plan is not an approvable plan.
pub const PROPOSAL_LIFETIME: Duration = Duration::from_secs(24 * 3600);

/// What a plan moves the workload to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanTarget {
    /// A new intent.
    Intent(Uuid),
    /// A new power declaration, by version.
    Power(u64),
    /// Retirement; volumes are retained.
    Retire,
    /// Another node, with the intent unchanged.
    Migrate,
    /// Deleting this retained volume, which no workload holds; the plan's workload is the one
    /// that created it (§7 of docs/06-storage.md).
    DeleteVolume(Uuid),
}

impl PlanTarget {
    /// The stored kind.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Intent(_) => "intent",
            Self::Power(_) => "power",
            Self::Retire => "retire",
            Self::Migrate => "migrate",
            Self::DeleteVolume(_) => "delete_volume",
        }
    }
}

/// A plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Immutable identity.
    pub id: Uuid,
    /// The workload.
    pub workload: Uuid,
    /// What it moves the workload to.
    pub target: PlanTarget,
    /// The operation that generated it, if any (§3 of docs/05-scheduling.md).
    pub operation: Option<Uuid>,
    /// The workload's target intent when the plan was computed; the plan is valid only
    /// against it.
    pub from_intent: Option<Uuid>,
    /// The workload's power target when the plan was computed, by version; zero for none.
    pub from_power: u64,
    /// When it was computed.
    pub computed_at: SystemTime,
    /// Who it was computed for.
    pub computed_by: Principal,
    /// The grant it was proposed under.
    pub proposed_under: Uuid,
    /// The steps, in order.
    pub steps: Vec<Step>,
    /// What it costs.
    pub impact: Impact,
    /// What it needs before it applies.
    pub approval: Approval,
    /// Why, in words.
    pub approval_reason: String,
    /// Where it is in its life.
    pub status: PlanStatus,
    /// The last step whose completion effect has been ingested.
    pub completed: Option<u32>,
    /// Approved now, applied in a window.
    pub apply_after: Option<SystemTime>,
    /// When it stops being approvable.
    pub expires_at: SystemTime,
    /// Who approved or confirmed it.
    pub approved_by: Option<Principal>,
    /// When.
    pub approved_at: Option<SystemTime>,
}

/// A transition the plan's status does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {from:?} plan cannot become {to:?}")]
pub struct TransitionError {
    /// The status it has.
    pub from: PlanStatus,
    /// The status asked for.
    pub to: PlanStatus,
}

impl Plan {
    /// Moves the plan to `to` if its status allows it.
    pub fn transition(&mut self, to: PlanStatus) -> Result<(), TransitionError> {
        if !self.status.may_become(to) {
            return Err(TransitionError { from: self.status, to });
        }
        self.status = to;
        Ok(())
    }

    /// Whether it can still be approved at `now`.
    #[must_use]
    pub fn approvable(&self, now: SystemTime) -> bool {
        self.status == PlanStatus::Proposed && now < self.expires_at
    }
}
