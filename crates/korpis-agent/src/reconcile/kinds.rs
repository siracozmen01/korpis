// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each of the node's drivers is, as each declared itself when the agent started: whether
//! it takes its volumes as block devices (§3.1 of docs/06-storage.md), and whether its workloads
//! hold a tap rather than a veth's end (§4.1 of docs/07-networking.md). What a driver is does not
//! change while it runs, and asking it again for each volume or network would tie every one of
//! them to the driver's own health.

use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};

/// What a driver is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Kind {
    block_device: bool,
    tap: bool,
    /// The cgroup its objects live under, for dedicated cores (§5.2 of docs/05-scheduling.md);
    /// empty when it declares none.
    cgroup_parent: String,
}

/// For each driver the node runs, by name, what it is.
#[derive(Debug, Clone, Default)]
pub struct DriverKinds(BTreeMap<String, Kind>);

impl DriverKinds {
    /// What `drivers` declared.
    #[must_use]
    pub fn of(drivers: &[pb::DriverInfo]) -> Self {
        let block = pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE;
        let kind = |d: &pb::DriverInfo| {
            let caps = d.capabilities.as_option();
            let modes = caps.map(|c| c.network_modes.as_slice()).unwrap_or_default();
            Kind {
                block_device: caps.is_some_and(|c| c.volume_semantics.as_known() == Some(block)),
                tap: modes.iter().any(|m| m.as_known() == Some(pb::NetworkMode::NETWORK_MODE_TAP)),
                cgroup_parent: caps.map(|c| c.cgroup_parent.clone()).unwrap_or_default(),
            }
        };
        Self(drivers.iter().map(|d| (d.name.clone(), kind(d))).collect())
    }

    /// The cgroups the node's drivers make their objects under, each once, whose cpusets the
    /// helper keeps for dedicated cores (§5.2 of docs/05-scheduling.md).
    #[must_use]
    pub fn cgroup_parents(&self) -> BTreeSet<String> {
        self.0.values().map(|k| k.cgroup_parent.clone()).filter(|p| !p.is_empty()).collect()
    }

    /// The cgroup the objects of the driver named `driver` live under, if it declares one.
    #[must_use]
    pub fn cgroup_parent(&self, driver: &str) -> Option<String> {
        Some(self.kind(driver).cgroup_parent).filter(|p| !p.is_empty())
    }

    /// The driver named `driver`, or the node's only one when none is named; a driver the node
    /// lacks is neither, and is refused when it is reached.
    fn kind(&self, driver: &str) -> Kind {
        match driver {
            "" if self.0.len() == 1 => self.0.values().next().cloned().unwrap_or_default(),
            name => self.0.get(name).cloned().unwrap_or_default(),
        }
    }

    /// Whether the driver named `driver` takes its volumes as block devices.
    #[must_use]
    pub fn block_device(&self, driver: &str) -> bool {
        self.kind(driver).block_device
    }

    /// Whether the workloads of the driver named `driver` hold a tap: a microVM's guest.
    #[must_use]
    pub fn tap(&self, driver: &str) -> bool {
        self.kind(driver).tap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver(name: &str, semantics: pb::VolumeSemantics, mode: pb::NetworkMode) -> pb::DriverInfo {
        let capabilities = pb::Capabilities {
            volume_semantics: semantics.into(),
            network_modes: vec![mode.into(), pb::NetworkMode::NETWORK_MODE_NONE.into()],
            ..Default::default()
        };
        pb::DriverInfo {
            name: name.into(),
            capabilities: capabilities.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_volume_is_a_block_device_and_a_link_a_tap_where_the_driver_says_so() {
        let block = pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE;
        let files = pb::VolumeSemantics::VOLUME_SEMANTICS_FILESYSTEM;
        let (veth, tap) = (pb::NetworkMode::NETWORK_MODE_VETH, pb::NetworkMode::NETWORK_MODE_TAP);
        let both =
            DriverKinds::of(&[driver("oci", files, veth), driver("firecracker", block, tap)]);
        assert!(both.block_device("firecracker") && !both.block_device("oci"));
        assert!(both.tap("firecracker") && !both.tap("oci"));
        assert!(!both.block_device("") && !both.tap(""), "with two drivers, none is named");
        assert!(!both.block_device("qemu"), "a driver the node lacks takes files");
        let one = DriverKinds::of(&[driver("firecracker", block, tap)]);
        assert!(one.block_device("") && one.tap(""), "the only driver, when none is named");
    }
}
