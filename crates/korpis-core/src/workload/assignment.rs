// SPDX-License-Identifier: AGPL-3.0-or-later

//! The assignment (§4.8 of docs/02-architecture.md): everything a node is told about one
//! workload placed on it. It is desired state, never an instruction: the node makes it true, and
//! a lost message is repaired by the next set, which carries the whole truth again.

use super::plan::step::{Step, StepExecutor};
use super::plan::{Plan, PlanTarget};
use super::spec::Continuity;
use super::{Intent, PowerRecord};
use uuid::Uuid;

/// One workload's desired state on one node.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// The workload.
    pub workload: Uuid,
    /// Its project.
    pub project: Uuid,
    /// Its organization.
    pub organization: Uuid,
    /// Monotonic per workload and node; any change below increments it.
    pub generation: u64,
    /// The approved intent in full: the workload's target, never its head.
    pub target: Intent,
    /// The approved power declaration: the workload's power target, never the newest.
    pub power: PowerRecord,
    /// What the node does when the lease ends: resolved, never [`Continuity::Derived`].
    pub on_lease_expiry: Continuity,
    /// The steps of the applying plan this node executes, while there are any.
    pub plan: Option<NodePlan>,
    /// The retire plan, when the workload is to be removed from this node.
    pub retire: Option<Uuid>,
    /// The resolved recipe document the target names, so the node needs no registry to know
    /// what to run.
    pub recipe_manifest: Vec<u8>,
    /// The node.
    pub node: Uuid,
    /// The lease epoch under which the node may act for the workload; zero until it holds one.
    pub lease_epoch: u64,
}

/// The steps of a plan a node executes (§4.8, §4.9 of docs/02-architecture.md).
#[derive(Debug, Clone, PartialEq)]
pub struct NodePlan {
    /// The plan.
    pub plan: Uuid,
    /// Its steps whose executor is the node, in order, each with its index.
    pub steps: Vec<Step>,
    /// The last step index the node may execute; cancelling lowers it.
    pub through: u32,
}

impl NodePlan {
    /// The node's part of `plan`, if it has one: its node steps, up to the last of them.
    #[must_use]
    pub fn of(plan: &Plan) -> Option<Self> {
        let steps: Vec<Step> =
            plan.steps.iter().filter(|s| s.executor == StepExecutor::Node).cloned().collect();
        let through = steps.last()?.index;
        Some(Self { plan: plan.id, steps, through })
    }
}

/// Whether `plan` retires the workload.
#[must_use]
pub fn retires(plan: &Plan) -> bool {
    matches!(plan.target, PlanTarget::Retire)
}

impl Continuity {
    /// What a node does when the lease ends, resolved: a declared answer stands, and a derived
    /// one is `Stop` exactly when a volume is in a class more than one node can attach (§4.5 of
    /// docs/02-architecture.md).
    #[must_use]
    pub fn resolved(self, multi_node_volume: bool) -> Self {
        match self {
            Self::Derived if multi_node_volume => Self::Stop,
            Self::Derived => Self::KeepRunning,
            declared => declared,
        }
    }
}

/// The digest of an assignment set (§9 of docs/10-api.md): over each assignment's workload and
/// generation, sorted by workload, as `sha256:` and 64 hexadecimal digits. The agent computes the
/// same over what it holds, and a match means nothing needs to be sent.
#[cfg(feature = "identity")]
#[must_use]
pub fn set_digest(entries: &[(Uuid, u64)]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut sorted = entries.to_vec();
    sorted.sort_unstable();
    let mut hasher = Sha256::new();
    for (workload, generation) in sorted {
        hasher.update(format!("{}:{generation}\n", workload.as_hyphenated()).as_bytes());
    }
    hasher.finalize().iter().fold(String::from("sha256:"), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_continuity_stops_only_with_a_shared_volume() {
        assert_eq!(Continuity::Derived.resolved(false), Continuity::KeepRunning);
        assert_eq!(Continuity::Derived.resolved(true), Continuity::Stop);
        assert_eq!(Continuity::KeepRunning.resolved(true), Continuity::KeepRunning);
    }

    #[cfg(feature = "identity")]
    #[test]
    fn a_set_digest_is_over_the_set_whatever_its_order() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(set_digest(&[(a, 1), (b, 3)]), set_digest(&[(b, 3), (a, 1)]));
        assert_ne!(set_digest(&[(a, 1), (b, 3)]), set_digest(&[(a, 2), (b, 3)]));
        assert!(set_digest(&[]).starts_with("sha256:"));
    }
}
