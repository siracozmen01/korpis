// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a workload may run, as its intent declares it (docs/05-scheduling.md).

use super::SpecError;
use crate::labels::LabelSelector;

/// A taint the workload tolerates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toleration {
    /// The taint key.
    pub key: String,
    /// The taint value; empty tolerates any value.
    pub value: String,
}

/// Placement relative to other workloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffinityRule {
    /// Keep apart rather than together.
    pub anti: bool,
    /// A requirement rather than a preference.
    pub required: bool,
    /// The other workloads.
    pub workloads: LabelSelector,
    /// The node label that defines "together", e.g. `rack`, `region`, `host`.
    pub topology_key: String,
}

/// The intent's placement constraints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementSpec {
    /// Nodes must match.
    pub node_selector: LabelSelector,
    /// Taints it tolerates.
    pub tolerations: Vec<Toleration>,
    /// Relative placement.
    pub affinity: Vec<AffinityRule>,
    /// A region preferred, not required.
    pub preferred_region: String,
}

impl PlacementSpec {
    /// Checks the constraints' form.
    pub fn validate(&self) -> Result<(), SpecError> {
        if let Some(i) = self.tolerations.iter().position(|t| t.key.is_empty()) {
            return Err(SpecError::new(&format!("placement.tolerations[{i}].key"), "required"));
        }
        if let Some(i) = self.affinity.iter().position(|a| a.topology_key.is_empty()) {
            return Err(SpecError::new(
                &format!("placement.affinity[{i}].topology_key"),
                "required",
            ));
        }
        Ok(())
    }
}
