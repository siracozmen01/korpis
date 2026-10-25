// SPDX-License-Identifier: AGPL-3.0-or-later

//! The assignment (§4.8 of docs/02-architecture.md): everything a node is told about one
//! workload placed on it. It is desired state, never an instruction: the node makes it true, and
//! a lost message is repaired by the next set, which carries the whole truth again.

use super::plan::step::{Step, StepExecutor};
use super::plan::{Plan, PlanStatus, PlanTarget};
use super::spec::Continuity;
use super::{Intent, PowerRecord};
use std::collections::BTreeMap;
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
    /// Its name, which the node does not use: what a restore that lost the workload recovers it
    /// under (§8 of docs/03-state.md).
    pub name: crate::tenancy::Name,
    /// Its labels, for the same reason.
    pub labels: crate::labels::Labels,
    /// Monotonic per workload and node; any change below increments it.
    pub generation: u64,
    /// The intent in full that the workload's plans reached, or that `plan` is reaching: never
    /// its head, and its target only once a plan reaches it (§4.8 of docs/02-architecture.md).
    pub target: Intent,
    /// Likewise for power; stopped at version zero while no plan has brought the workload to
    /// any intent.
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
    /// The host ids the workload's user namespace maps onto (§4.5 of docs/04-runtimes.md).
    pub ids: IdRange,
    /// A retired workload's residual assignment: it carries only a plan for what the workload
    /// left on the node, and the node executes that plan and nothing else (§7 of
    /// docs/06-storage.md).
    pub residual: bool,
    /// The helper's identifier of each existing volume the target attaches, by the name the
    /// workload mounts it under (§7 of docs/06-storage.md).
    pub volume_ids: BTreeMap<String, String>,
    /// The target's endpoints as placed on this node, each with the port of the node it holds
    /// where it holds one (§3 of docs/07-networking.md); sorted by name.
    pub endpoints: Vec<super::endpoint::Endpoint>,
    /// Whether the node relays the control plane's API to the workload: it runs an installed
    /// extension, or holds a grant in force (§9.5 of docs/08-identity.md).
    pub reaches_api: bool,
    /// What a spent egress budget holds it to this month, if one does (§5.1 of
    /// docs/05-scheduling.md, "The egress budget").
    pub egress: Option<crate::tenancy::quota::Exhaustion>,
    /// Its dependencies, resolved to the endpoints they reach now (§5 of
    /// docs/07-networking.md).
    pub dependencies: Vec<super::dependency::ResolvedDependency>,
    /// The HTTP-01 challenges the node's proxy answers for its endpoints' names while their
    /// certificates are ordered (§3.2 of docs/07-networking.md).
    pub challenges: Vec<crate::dns::certificate::HttpChallenge>,
    /// The TLS-ALPN-01 challenges it answers for them on port 443, by name.
    pub alpn_challenges: Vec<crate::dns::certificate::AlpnChallenge>,
    /// The runs of a task or scheduled workload the node is to execute (§4.5 of
    /// docs/05-scheduling.md).
    pub runs: Vec<super::run::RunRef>,
    /// Every run of it the control plane still holds, in any status: the node keeps their
    /// streams, and deletes another run's only because it is not here, which is the control
    /// plane saying retention deleted it (§4.4 of docs/05-scheduling.md, §4 of
    /// docs/14-streams.md).
    pub kept_runs: Vec<Uuid>,
    /// The node's devices its placement there holds, which the node gives its driver (§6.3 of
    /// docs/05-scheduling.md).
    pub devices: Vec<crate::node::device::DeviceHold>,
}

/// How many ids a workload's user namespace maps: the workload sees 0 to 65535.
pub const IDS_PER_WORKLOAD: u32 = 1 << 16;
/// The first host id given to workloads: above every id a host's own users and the subordinate
/// ranges of its rootless users conventionally take.
pub const FIRST_WORKLOAD_ID: u32 = 1 << 30;

/// A range of host ids, for users and groups alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdRange {
    /// The host id the workload's id 0 maps to.
    pub base: u32,
    /// How many ids.
    pub count: u32,
}

impl IdRange {
    /// The range of node slot `slot`, if the id space has one: slots are consecutive ranges of
    /// [`IDS_PER_WORKLOAD`] from [`FIRST_WORKLOAD_ID`], and the last valid id is `u32::MAX - 1`,
    /// since `u32::MAX` is `-1` to the kernel.
    #[must_use]
    pub fn of_slot(slot: u32) -> Option<Self> {
        let base = slot.checked_mul(IDS_PER_WORKLOAD)?.checked_add(FIRST_WORKLOAD_ID)?;
        let last = base.checked_add(IDS_PER_WORKLOAD - 1)?;
        (last < u32::MAX).then_some(Self { base, count: IDS_PER_WORKLOAD })
    }

    /// How many slots a node has.
    #[must_use]
    pub fn slots() -> u32 {
        (u32::MAX - FIRST_WORKLOAD_ID) / IDS_PER_WORKLOAD
    }

    /// The slot this range is, if it is exactly one: the only ranges a workload is ever given,
    /// so anything else, a range reaching a host's own ids above all, is not a workload's.
    #[must_use]
    pub fn slot(self) -> Option<u32> {
        let offset = self.base.checked_sub(FIRST_WORKLOAD_ID)?;
        let slot = offset / IDS_PER_WORKLOAD;
        (Self::of_slot(slot) == Some(self)).then_some(slot)
    }
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
    /// The plan in full, which the node does not use: what a restore that lost it recovers it
    /// from (§8 of docs/03-state.md). Its status is applying and its progress none whatever
    /// they are, so that neither changes what the node is told.
    pub whole: Plan,
}

impl NodePlan {
    /// The node's part of `plan`, if it has one now: its node steps, through the last of them
    /// before the first control-plane step not yet completed, which the node waits for (§4.9 of
    /// docs/02-architecture.md). A plan whose next step is the control plane's, with no node step
    /// before it left, is no node's yet.
    #[must_use]
    pub fn of(plan: &Plan) -> Option<Self> {
        let pending = |s: &&Step| plan.completed.is_none_or(|c| s.index > c);
        let waits_at = plan
            .steps
            .iter()
            .filter(pending)
            .find(|s| s.executor == StepExecutor::ControlPlane)
            .map(|s| s.index);
        let steps: Vec<Step> =
            plan.steps.iter().filter(|s| s.executor == StepExecutor::Node).cloned().collect();
        let through = steps.iter().rfind(|s| waits_at.is_none_or(|w| s.index < w))?.index;
        let whole = Plan { status: PlanStatus::Applying, completed: None, ..plan.clone() };
        Some(Self { plan: plan.id, steps, through, whole })
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

    #[test]
    fn slots_tile_the_id_space_above_the_host_and_stop_short_of_minus_one() {
        assert_eq!(IdRange::of_slot(0), Some(IdRange { base: 1 << 30, count: 65536 }));
        let last = IdRange::slots() - 1;
        let top = IdRange::of_slot(last).unwrap();
        assert!(top.base.checked_add(top.count - 1).is_some_and(|l| l < u32::MAX));
        assert_eq!(IdRange::of_slot(last + 1), None);
        assert_eq!(IdRange::of_slot(u32::MAX), None);
        assert_eq!(top.slot(), Some(last));
        assert_eq!(IdRange { base: 0, count: 65536 }.slot(), None, "a host's own ids");
        assert_eq!(IdRange { base: (1 << 30) + 1, count: 65536 }.slot(), None, "unaligned");
        assert_eq!(IdRange { base: 1 << 30, count: 1 << 17 }.slot(), None, "two slots wide");
    }

    fn plan_of(executors: &[StepExecutor], completed: Option<u32>) -> Plan {
        use crate::object::{Kind, ObjectRef};
        use crate::principal::Principal;
        use crate::workload::plan::impact::{Estimate, Impact, ResourceDelta};
        use crate::workload::plan::step::{Layer, StepAction};
        use crate::workload::plan::{Approval, PlanStatus};
        let step = |(i, e): (usize, &StepExecutor)| Step {
            index: u32::try_from(i).unwrap(),
            action: StepAction::Start,
            executor: *e,
            target: ObjectRef::new(Kind::Workload, Uuid::nil()),
            reversible: true,
            disruptive: false,
            classified_by: Layer::Vocabulary,
            reason: String::new(),
            detail: crate::value::ValueMap::new(),
            timeout: None,
        };
        let now = std::time::SystemTime::UNIX_EPOCH;
        Plan {
            id: Uuid::from_u128(7),
            workload: Uuid::nil(),
            target: PlanTarget::Retire,
            operation: None,
            from_intent: None,
            from_power: 0,
            computed_at: now,
            computed_by: Principal::User(Uuid::nil()),
            proposed_under: None,
            steps: executors.iter().enumerate().map(step).collect(),
            impact: Impact {
                disruptive: false,
                downtime: Estimate::Unknown,
                data_at_risk: Estimate::Unknown,
                cost_delta: ResourceDelta::default(),
                exposures: Vec::new(),
            },
            approval: Approval::Auto,
            approval_reason: String::new(),
            status: PlanStatus::Applying,
            completed,
            apply_after: None,
            expires_at: now,
            approved_by: None,
            approved_at: None,
        }
    }

    #[test]
    fn a_node_runs_up_to_a_control_plane_step_and_on_once_it_is_done() {
        use StepExecutor::{ControlPlane as C, Node as N};
        let through = |p: &Plan| NodePlan::of(p).map(|n| n.through);
        assert_eq!(through(&plan_of(&[N, N, C, N], None)), Some(1), "waits at step 2");
        assert_eq!(through(&plan_of(&[N, N, C, N], Some(1))), Some(1), "still waits");
        assert_eq!(through(&plan_of(&[N, N, C, N], Some(2))), Some(3), "and goes on");
        assert_eq!(through(&plan_of(&[C, N], None)), None, "nothing is the node's yet");
        assert_eq!(through(&plan_of(&[C], None)), None, "a plan the control plane runs alone");
        assert_eq!(through(&plan_of(&[N, N], None)), Some(1));
        assert_eq!(NodePlan::of(&plan_of(&[N, C, N], None)).unwrap().steps.len(), 2);
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
