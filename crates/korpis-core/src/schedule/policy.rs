// SPDX-License-Identifier: AGPL-3.0-or-later

//! The placement policy (§2.2 of docs/05-scheduling.md): how candidates that can run a workload
//! are ranked. Spread and affinity operate on topology labels and are orthogonal to the strategy.

use crate::labels::LabelSelector;
use crate::workload::spec::placement::AffinityRule;
use std::collections::BTreeMap;
use uuid::Uuid;

/// What utilization the scheduler prefers, in Agones' vocabulary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Strategy {
    /// The fullest node that still fits: consolidation, so elastic capacity can scale down.
    Packed,
    /// The emptiest node: fixed hardware, the smallest blast radius.
    Distributed,
    /// The node whose most loaded dimension stays lowest, which evens utilization across nodes
    /// and across dimensions.
    #[default]
    Balanced,
}

/// Which workloads a spread constraint counts. Always within the placed workload's own
/// organization: a constraint never counts, or reveals, another tenant's workloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpreadScope {
    /// The workloads of the placed workload's project.
    Project,
    /// The workloads of its organization.
    Organization,
    /// The workloads of its organization that the selector matches; a workload it does not
    /// match is not constrained by it.
    Selector(LabelSelector),
}

/// "No more than `max_skew` more of these workloads in one domain than in another."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpreadConstraint {
    /// The node label that defines a domain: `rack`, `region`, `power_feed`; `host` is the
    /// node itself.
    pub topology_key: String,
    /// The largest difference allowed between the fullest and the emptiest domain.
    pub max_skew: u32,
    /// The workloads it counts.
    pub scope: SpreadScope,
}

/// A scoring signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    /// Utilization after placement, per the strategy.
    Utilization,
    /// The node already holds the recipe's content.
    ContentLocality,
    /// The node already holds the workload's volumes.
    VolumeLocality,
    /// A GPU node is not spent on a workload that needs none.
    DeviceFit,
    /// Spread constraints, preferred affinity, and the preferred region.
    FailureDomain,
    /// The node did not recently lose workloads.
    Churn,
}

impl Signal {
    /// Every signal.
    pub const ALL: [Self; 6] = [
        Self::Utilization,
        Self::ContentLocality,
        Self::VolumeLocality,
        Self::DeviceFit,
        Self::FailureDomain,
        Self::Churn,
    ];

    /// Its weight where a policy sets none. Content locality weighs a quarter, so it decides
    /// between nodes whose utilization is close and never overrides the strategy: at the
    /// weight of utilization, a node holding a popular recipe would take every workload of it
    /// whatever the strategy said (question 3 of §10 of docs/05-scheduling.md). Every other
    /// signal weighs 1.
    #[must_use]
    pub const fn default_weight(self) -> f64 {
        match self {
            Self::ContentLocality => 0.25,
            _ => 1.0,
        }
    }

    /// Its name in an explanation and in a policy's weights.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utilization => "utilization",
            Self::ContentLocality => "content_locality",
            Self::VolumeLocality => "volume_locality",
            Self::DeviceFit => "device_fit",
            Self::FailureDomain => "failure_domain",
            Self::Churn => "churn",
        }
    }

    /// The signal a name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// The most spread constraints a placement policy states, for the reason affinity rules are
/// bounded ([`crate::workload::spec::placement::MAX_AFFINITY`]).
pub const MAX_SPREAD: usize = 16;

/// How candidates are ranked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlacementPolicy {
    /// The utilization preferred.
    pub strategy: Strategy,
    /// Spread constraints.
    pub spread: Vec<SpreadConstraint>,
    /// Affinity rules applied to every workload, beside its own.
    pub affinity: Vec<AffinityRule>,
    /// Per signal; a signal absent weighs [`Signal::default_weight`].
    pub weights: BTreeMap<Signal, f64>,
}

impl PlacementPolicy {
    /// The weight of `signal`.
    #[must_use]
    pub fn weight(&self, signal: Signal) -> f64 {
        self.weights.get(&signal).copied().unwrap_or_else(|| signal.default_weight())
    }
}

/// The policies one placement is decided by (§2.2 of docs/05-scheduling.md): the workload's,
/// whose spread constraints and affinity rules it is placed under, and each node owner's, whose
/// strategy and weights its nodes are scored by. Nodes are compared within the nearest owner
/// first, so no two policies' scores are ever compared.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policies {
    /// The policy nearest the workload's own organization, or the installation's.
    pub workload: PlacementPolicy,
    /// Per node owner, by its organization: the policy nearest it, whose strategy and weights
    /// score its nodes.
    pub owners: BTreeMap<Uuid, PlacementPolicy>,
}

impl Policies {
    /// One policy for everything, as an installation where no organization set one decides.
    #[must_use]
    pub fn single(policy: PlacementPolicy) -> Self {
        Self { workload: policy, owners: BTreeMap::new() }
    }

    /// The policy `owner`'s nodes are scored by: the workload's for an owner it was not told
    /// of.
    #[must_use]
    pub fn of_owner(&self, owner: Uuid) -> &PlacementPolicy {
        self.owners.get(&owner).unwrap_or(&self.workload)
    }
}
