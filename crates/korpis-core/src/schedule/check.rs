// SPDX-License-Identifier: AGPL-3.0-or-later

//! The checks a candidate node is filtered by (§2.1 of docs/05-scheduling.md).

/// A filter check (§2.1 of docs/05-scheduling.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    /// The node serves the workload's organization.
    Owner,
    /// The node is ready.
    Status,
    /// A healthy driver of the requested name.
    Driver,
    /// The driver implements the requested tier.
    Tier,
    /// The driver offers the requested console.
    Console,
    /// The driver can run every probe the intent declares that needs it (§5 of
    /// docs/04-runtimes.md).
    Probe,
    /// The driver holds every limit the intent sets (Rule K-3).
    Enforcement,
    /// The node can be reached the way the endpoints need.
    Reachability,
    /// The intent's node selector.
    Selector,
    /// Every taint is tolerated.
    Taint,
    /// Unallocated capacity for every reservation.
    Capacity,
    /// Free devices for every request, of a class and mode its driver gives (§6.3 of
    /// docs/05-scheduling.md).
    Device,
    /// Enough free cores offered for dedication, where they are asked for (§5.2 of
    /// docs/05-scheduling.md).
    Cores,
    /// Every named storage class is offered.
    StorageClass,
    /// Every existing volume attached is on the node.
    Volume,
    /// Every required affinity and anti-affinity rule.
    Affinity,
    /// Every dependency's targets are reachable from the node (§5 of docs/07-networking.md).
    Dependency,
    /// It may hold a dedicated address there, where an endpoint of it is `dedicated_ip` (§3.7
    /// of docs/07-networking.md).
    DedicatedIp,
    /// It reaches every member of the edge group through the mesh, where an endpoint of it
    /// resolves to `stable` (§3.1 of docs/07-networking.md).
    Edge,
    /// Every quota entry counting what runs on the node admits it (§5.1 of
    /// docs/05-scheduling.md).
    Quota,
    /// The node's driver accepted the intent when the shortlist asked it, or after it was bound
    /// there (§2.1 of docs/05-scheduling.md).
    Prepare,
}

impl Check {
    /// Every check, in the order they are evaluated.
    pub const ALL: [Self; 21] = [
        Self::Owner,
        Self::Status,
        Self::Driver,
        Self::Tier,
        Self::Console,
        Self::Probe,
        Self::Enforcement,
        Self::Reachability,
        Self::Selector,
        Self::Taint,
        Self::Capacity,
        Self::Device,
        Self::Cores,
        Self::StorageClass,
        Self::Volume,
        Self::Affinity,
        Self::Dependency,
        Self::DedicatedIp,
        Self::Edge,
        Self::Quota,
        Self::Prepare,
    ];

    /// The check a name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Its name in an explanation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Status => "status",
            Self::Driver => "driver",
            Self::Tier => "tier",
            Self::Console => "console",
            Self::Probe => "probe",
            Self::Enforcement => "enforcement",
            Self::Reachability => "reachability",
            Self::Selector => "selector",
            Self::Taint => "taint",
            Self::Capacity => "capacity",
            Self::Device => "device",
            Self::Cores => "cores",
            Self::StorageClass => "storage_class",
            Self::Volume => "volume",
            Self::Affinity => "affinity",
            Self::Dependency => "dependency",
            Self::DedicatedIp => "dedicated_ip",
            Self::Edge => "edge",
            Self::Quota => "quota",
            Self::Prepare => "prepare",
        }
    }
}
