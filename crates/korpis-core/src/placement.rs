// SPDX-License-Identifier: AGPL-3.0-or-later

//! The binding of a workload to a node, and why.

use crate::schedule::Explanation;
use crate::tenancy::quota::Bounds;
use std::time::SystemTime;
use uuid::Uuid;

/// Who bound it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundBy {
    /// The scheduler, by the policy.
    Scheduler,
    /// An operator, overriding it; the reason is theirs.
    Operator,
    /// A migration, during cutover.
    Migration,
    /// A restore, to the node that presented the workload's assignment.
    Recovery,
    /// Failover: placed again once its node went silent and its lease ended.
    Failover,
}

impl BoundBy {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduler => "scheduler",
            Self::Operator => "operator",
            Self::Migration => "migration",
            Self::Recovery => "recovery",
            Self::Failover => "failover",
        }
    }

    /// The value a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Scheduler, Self::Operator, Self::Migration, Self::Recovery, Self::Failover]
            .into_iter()
            .find(|b| b.as_str() == s)
    }
}

/// A workload bound to a node.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// The workload.
    pub workload: Uuid,
    /// The node.
    pub node: Uuid,
    /// The approved intent it was bound for; when the target moves on and still fits here, the
    /// placement follows it in place.
    pub intent: Uuid,
    /// What it reserves on the node: that intent's reservations.
    pub reserved: Bounds,
    /// When.
    pub bound_at: SystemTime,
    /// By whom.
    pub bound_by: BoundBy,
    /// Why this node, for a human.
    pub reason: String,
    /// The full derivation.
    pub explanation: Explanation,
    /// During a migration two placements coexist and exactly one is authoritative.
    pub authoritative: bool,
    /// When a restore held it: its node presented its assignments without this one, so nothing is
    /// sent for it until an operator confirms it or it is retired.
    pub held_at: Option<SystemTime>,
    /// The node's devices it holds, chosen when it was bound.
    pub devices: Vec<crate::node::device::DeviceHold>,
    /// The node's cores it holds, chosen when it was bound for dedicated cores.
    pub cores: Vec<u32>,
}

/// The newest attempt that found no node for a workload's target.
#[derive(Debug, Clone, PartialEq)]
pub struct Unplaced {
    /// The workload.
    pub workload: Uuid,
    /// The approved intent no node could take.
    pub intent: Uuid,
    /// When this explanation was first reached. A retry that reaches the same one changes
    /// nothing, so a watcher is told when the reason changes, not every time it is retried.
    pub since: SystemTime,
    /// Why every candidate was rejected.
    pub explanation: Explanation,
}
