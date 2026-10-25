// SPDX-License-Identifier: AGPL-3.0-or-later

//! The binding of a workload to a node, and why (§3.4 of docs/01-model.md, §2.3 of
//! docs/05-scheduling.md).

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
    /// A restore, to the node that presented the workload's assignment (§8 of
    /// docs/03-state.md).
    Recovery,
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
        }
    }

    /// The value a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Scheduler, Self::Operator, Self::Migration, Self::Recovery]
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
    /// What it reserves on the node: that intent's reservations (§5.2 of
    /// docs/05-scheduling.md).
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
    /// When a restore held it: its node presented its assignments without this one, so nothing
    /// is sent for it until an operator confirms it or it is retired (§8 of docs/03-state.md).
    pub held_at: Option<SystemTime>,
    /// The node's devices it holds, chosen when it was bound (§6.3 of docs/05-scheduling.md).
    pub devices: Vec<crate::node::device::DeviceHold>,
    /// The node's cores it holds, chosen when it was bound for dedicated cores (§5.2 of
    /// docs/05-scheduling.md).
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
