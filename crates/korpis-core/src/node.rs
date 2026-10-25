// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node (§3.4 of docs/01-model.md): a machine running the Korpis agent, owned by the
//! organization it was enrolled into, with two independent states. What the operator decided
//! (`admin`) and whether the control plane can hear it (`connectivity`) change for different
//! reasons, so they are kept apart and the status a client shows is derived from both.

pub mod advertised;
pub mod cores;
pub mod device;
pub mod driver;
#[cfg(feature = "identity")]
pub mod identity;
pub mod journal;
pub mod mesh;
pub mod pool;

pub use driver::{Capabilities, DriverInfo, Enforcement, StorageClass};

use crate::labels::Labels;
use crate::tenancy::Name;
use crate::tenancy::quota::Bounds;
use std::time::SystemTime;
use uuid::Uuid;

/// A taint: workloads must tolerate it to land on the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taint {
    /// The key.
    pub key: String,
    /// The value.
    pub value: String,
}

/// What the operator decided about the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeAdmin {
    /// Takes new placements.
    Active,
    /// Takes no new placements; what runs there keeps running.
    Cordoned,
    /// Everything is being moved off, by an operation (§3 of docs/05-scheduling.md).
    Draining,
    /// Retired: empty, and never used again.
    Decommissioned,
}

/// Whether the control plane can hear the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    /// Enrolled, and its agent has not yet completed a session.
    Registering,
    /// In session.
    Connected,
    /// Its session ended and it has not come back within the staleness window.
    Unreachable,
    /// Nothing is known: the control plane has not heard from it since it started.
    Unknown,
}

/// The single status a client shows, derived from the two states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    /// Enrolled, not yet heard from.
    Registering,
    /// Active and connected: the only status that takes new placements.
    Ready,
    /// Connected and cordoned.
    Cordoned,
    /// Connected and draining.
    Draining,
    /// Not heard from within the staleness window, whatever the operator decided.
    Unreachable,
    /// Not heard from since the control plane started.
    Unknown,
    /// Retired.
    Decommissioned,
}

impl NodeStatus {
    /// Its name, as a client shows it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registering => "registering",
            Self::Ready => "ready",
            Self::Cordoned => "cordoned",
            Self::Draining => "draining",
            Self::Unreachable => "unreachable",
            Self::Unknown => "unknown",
            Self::Decommissioned => "decommissioned",
        }
    }
}

/// Whether a node can be reached for inbound traffic (§9 of docs/07-networking.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    /// A measured public address on the node itself.
    Public,
    /// Behind NAT, or not yet measured: inbound only through an edge.
    EdgeOnly,
}

/// A machine running the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Immutable.
    pub id: Uuid,
    /// For humans; unique in the installation.
    pub name: Name,
    /// The organization it was enrolled into. It serves that organization and every
    /// organization below it, and a grant over the organization covers it.
    pub organization: Uuid,
    /// Region, rack, arch, and the like: there is no `Location` object.
    pub labels: Labels,
    /// What workloads must tolerate.
    pub taints: Vec<Taint>,
    /// Total.
    pub capacity: Bounds,
    /// Capacity minus the system reserve: what placements may reserve.
    pub allocatable: Bounds,
    /// The runtime drivers it offers, with their declared capabilities.
    pub drivers: Vec<DriverInfo>,
    /// The devices its agent found described (§6.1 of docs/05-scheduling.md).
    pub devices: Vec<device::Device>,
    /// The devices described that it does not offer, and why.
    pub unoffered_devices: Vec<device::UnofferedDevice>,
    /// The physical cores it offers for dedication (§5.2 of docs/05-scheduling.md).
    pub cores: Vec<cores::Core>,
    /// The storage classes it offers.
    pub storage_classes: Vec<StorageClass>,
    /// Measured inbound reachability.
    pub reachability: Reachability,
    /// What it advertises for inbound traffic; none until it says.
    pub advertised: Option<advertised::Advertised>,
    /// When its reachability was last measured; none since its addresses last changed.
    pub probed_at: Option<SystemTime>,
    /// What the operator decided.
    pub admin: NodeAdmin,
    /// Whether it is heard.
    pub connectivity: Connectivity,
    /// The agent's version, once it has said.
    pub agent_version: Option<String>,
    /// When it was last heard.
    pub last_seen: Option<SystemTime>,
    /// When it was enrolled.
    pub enrolled_at: SystemTime,
    /// Bumped on every change, for compare-and-swap.
    pub version: u64,
}

/// A change of the operator's decision the node's state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {from:?} node cannot become {to:?}")]
pub struct AdminError {
    /// The state it is in.
    pub from: NodeAdmin,
    /// The state asked for.
    pub to: NodeAdmin,
}

impl Node {
    /// The status a client shows. A connectivity problem outranks the operator's decision,
    /// because it is the one that needs attention; the decision is still in `admin`.
    #[must_use]
    pub fn status(&self) -> NodeStatus {
        match (self.admin, self.connectivity) {
            (NodeAdmin::Decommissioned, _) => NodeStatus::Decommissioned,
            (_, Connectivity::Registering) => NodeStatus::Registering,
            (_, Connectivity::Unreachable) => NodeStatus::Unreachable,
            (_, Connectivity::Unknown) => NodeStatus::Unknown,
            (NodeAdmin::Draining, Connectivity::Connected) => NodeStatus::Draining,
            (NodeAdmin::Cordoned, Connectivity::Connected) => NodeStatus::Cordoned,
            (NodeAdmin::Active, Connectivity::Connected) => NodeStatus::Ready,
        }
    }

    /// Moves the operator's decision to `to`: `active ⇄ cordoned` only. Draining and
    /// decommissioning are operations (§3 of docs/05-scheduling.md), not toggles, and a node in
    /// either leaves it only through its operation. Returns whether anything changed.
    pub fn set_admin(&mut self, to: NodeAdmin) -> Result<bool, AdminError> {
        use NodeAdmin as A;
        match (self.admin, to) {
            (from, to) if from == to => Ok(false),
            (A::Active, A::Cordoned) | (A::Cordoned, A::Active) => {
                self.admin = to;
                Ok(true)
            }
            (from, to) => Err(AdminError { from, to }),
        }
    }

    /// Retires the node for good. A node is removed from `cordoned`, once it holds nothing,
    /// which the caller checks; one removed as `compromised` is removed from any state, since
    /// nothing it holds can be trusted to leave by an operation it takes part in (§9 of
    /// docs/17-security.md). Returns whether anything changed.
    pub fn decommission(&mut self, compromised: bool) -> Result<bool, AdminError> {
        match self.admin {
            NodeAdmin::Decommissioned => Ok(false),
            NodeAdmin::Cordoned => {
                self.admin = NodeAdmin::Decommissioned;
                Ok(true)
            }
            _ if compromised => {
                self.admin = NodeAdmin::Decommissioned;
                Ok(true)
            }
            from => Err(AdminError { from, to: NodeAdmin::Decommissioned }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(admin: NodeAdmin, connectivity: Connectivity) -> Node {
        Node {
            id: Uuid::nil(),
            name: Name::parse("fra-3").unwrap(),
            organization: Uuid::nil(),
            labels: Labels::default(),
            taints: Vec::new(),
            capacity: Bounds::new(),
            allocatable: Bounds::new(),
            drivers: Vec::new(),
            devices: Vec::new(),
            unoffered_devices: Vec::new(),
            cores: Vec::new(),
            storage_classes: Vec::new(),
            reachability: Reachability::EdgeOnly,
            advertised: None,
            probed_at: None,
            admin,
            connectivity,
            agent_version: None,
            last_seen: None,
            enrolled_at: SystemTime::UNIX_EPOCH,
            version: 1,
        }
    }

    #[test]
    fn a_cordoned_node_that_drops_out_and_returns_is_still_cordoned() {
        let mut n = node(NodeAdmin::Cordoned, Connectivity::Connected);
        assert_eq!(n.status(), NodeStatus::Cordoned);
        n.connectivity = Connectivity::Unreachable;
        assert_eq!(n.status(), NodeStatus::Unreachable);
        n.connectivity = Connectivity::Connected;
        assert_eq!(n.status(), NodeStatus::Cordoned, "reconnecting does not uncordon");
    }

    #[test]
    fn cordon_toggles_and_nothing_else_does() {
        let mut n = node(NodeAdmin::Active, Connectivity::Connected);
        assert_eq!(n.set_admin(NodeAdmin::Cordoned), Ok(true));
        assert_eq!(n.set_admin(NodeAdmin::Cordoned), Ok(false), "already cordoned");
        assert_eq!(n.set_admin(NodeAdmin::Active), Ok(true));
        assert!(n.set_admin(NodeAdmin::Draining).is_err(), "draining is an operation");
        let mut d = node(NodeAdmin::Draining, Connectivity::Connected);
        assert!(d.set_admin(NodeAdmin::Active).is_err(), "only its operation ends a drain");
    }

    #[test]
    fn a_node_is_removed_cordoned_or_as_compromised_and_never_comes_back() {
        let mut active = node(NodeAdmin::Active, Connectivity::Connected);
        assert!(active.decommission(false).is_err(), "cordoned first");
        assert_eq!(active.decommission(true), Ok(true), "a compromised one from any state");
        assert_eq!(active.status(), NodeStatus::Decommissioned);
        assert_eq!(active.decommission(true), Ok(false));
        assert!(active.set_admin(NodeAdmin::Active).is_err(), "never used again");
        let mut cordoned = node(NodeAdmin::Cordoned, Connectivity::Unreachable);
        assert_eq!(cordoned.decommission(false), Ok(true));
    }
}
