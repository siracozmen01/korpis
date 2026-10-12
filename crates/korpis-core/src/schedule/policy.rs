// SPDX-License-Identifier: AGPL-3.0-or-later

//! The placement policy (§2.2 of docs/05-scheduling.md): how candidates that can run a workload
//! are ranked. Spread and affinity operate on topology labels and are orthogonal to the strategy.

use crate::labels::LabelSelector;
use crate::workload::spec::placement::AffinityRule;
use std::collections::BTreeMap;

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

/// How candidates are ranked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlacementPolicy {
    /// The utilization preferred.
    pub strategy: Strategy,
    /// Spread constraints.
    pub spread: Vec<SpreadConstraint>,
    /// Affinity rules applied to every workload, beside its own.
    pub affinity: Vec<AffinityRule>,
    /// Per signal; a signal absent weighs 1.
    pub weights: BTreeMap<Signal, f64>,
}

impl PlacementPolicy {
    /// The weight of `signal`.
    #[must_use]
    pub fn weight(&self, signal: Signal) -> f64 {
        self.weights.get(&signal).copied().unwrap_or(1.0)
    }
}
