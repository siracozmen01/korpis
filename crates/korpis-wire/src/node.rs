// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nodes, their drivers and storage, and amounts per dimension.

mod cores;
mod devices;
mod facts;
mod overlay;
pub(crate) mod storage;

pub use cores::{core_to_pb, cores_from_pb};
pub use devices::{
    device_to_wire, devices_from_pb, holds_from_pb, holds_to_pb, mode_from_pb, mode_to_pb,
    unoffered_from_pb, unoffered_to_wire,
};
pub use facts::{drivers_from_pb, resources_from_pb};
use korpis_core::node::mesh::Standing;
use overlay::standing_into;
pub use overlay::{overlay_from_pb, overlay_to_pb, peers_to_pb};
pub use storage::{storage_class_to_pb, storage_classes_from_pb};

use crate::spec::{console_to_pb, tier_to_pb};
use crate::{Result, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::node::advertised::{Advertised, PortRange};
use korpis_core::node::driver::{
    DeviceClass, FileAccess, NetworkMode, RootfsSource, VolumeSemantics,
};
use korpis_core::node::{
    Capabilities, Connectivity, DriverInfo, Node, NodeAdmin, NodeStatus, Reachability,
};
use korpis_core::tenancy::quota::{Bounds, Dimension};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::net::IpAddr;

enum_map!(admin_to_pb, admin_from_pb, NodeAdmin, pb::NodeAdmin, "a node admin state", {
    NodeAdmin::Active => pb::NodeAdmin::NODE_ADMIN_ACTIVE,
    NodeAdmin::Cordoned => pb::NodeAdmin::NODE_ADMIN_CORDONED,
    NodeAdmin::Draining => pb::NodeAdmin::NODE_ADMIN_DRAINING,
    NodeAdmin::Decommissioned => pb::NodeAdmin::NODE_ADMIN_DECOMMISSIONED,
});

enum_map!(connectivity_to_pb, connectivity_from_pb, Connectivity, pb::NodeConnectivity,
    "a node connectivity", {
    Connectivity::Registering => pb::NodeConnectivity::NODE_CONNECTIVITY_REGISTERING,
    Connectivity::Connected => pb::NodeConnectivity::NODE_CONNECTIVITY_CONNECTED,
    Connectivity::Unreachable => pb::NodeConnectivity::NODE_CONNECTIVITY_UNREACHABLE,
    Connectivity::Unknown => pb::NodeConnectivity::NODE_CONNECTIVITY_UNKNOWN,
});

enum_map!(status_to_pb, status_from_pb, NodeStatus, pb::NodeStatus, "a node status", {
    NodeStatus::Registering => pb::NodeStatus::NODE_STATUS_REGISTERING,
    NodeStatus::Ready => pb::NodeStatus::NODE_STATUS_READY,
    NodeStatus::Cordoned => pb::NodeStatus::NODE_STATUS_CORDONED,
    NodeStatus::Draining => pb::NodeStatus::NODE_STATUS_DRAINING,
    NodeStatus::Unreachable => pb::NodeStatus::NODE_STATUS_UNREACHABLE,
    NodeStatus::Unknown => pb::NodeStatus::NODE_STATUS_UNKNOWN,
    NodeStatus::Decommissioned => pb::NodeStatus::NODE_STATUS_DECOMMISSIONED,
});

enum_map!(reachability_to_pb, reachability_from_pb, Reachability, pb::Reachability,
    "a reachability", {
    Reachability::Public => pb::Reachability::REACHABILITY_PUBLIC,
    Reachability::EdgeOnly => pb::Reachability::REACHABILITY_EDGE_ONLY,
});

enum_map!(device_to_pb, device_from_pb, DeviceClass, pb::DeviceClass, "a device class", {
    DeviceClass::Gpu => pb::DeviceClass::DEVICE_CLASS_GPU,
    DeviceClass::Usb => pb::DeviceClass::DEVICE_CLASS_USB,
    DeviceClass::Pci => pb::DeviceClass::DEVICE_CLASS_PCI,
    DeviceClass::Tpm => pb::DeviceClass::DEVICE_CLASS_TPM,
    DeviceClass::Serial => pb::DeviceClass::DEVICE_CLASS_SERIAL,
});

enum_map!(rootfs_to_pb, rootfs_from_pb, RootfsSource, pb::RootfsSource, "a rootfs source", {
    RootfsSource::OciImage => pb::RootfsSource::ROOTFS_SOURCE_OCI_IMAGE,
    RootfsSource::Directory => pb::RootfsSource::ROOTFS_SOURCE_DIRECTORY,
    RootfsSource::DiskImage => pb::RootfsSource::ROOTFS_SOURCE_DISK_IMAGE,
    RootfsSource::Iso => pb::RootfsSource::ROOTFS_SOURCE_ISO,
});

enum_map!(network_to_pb, network_from_pb, NetworkMode, pb::NetworkMode, "a network mode", {
    NetworkMode::Veth => pb::NetworkMode::NETWORK_MODE_VETH,
    NetworkMode::Tap => pb::NetworkMode::NETWORK_MODE_TAP,
    NetworkMode::Host => pb::NetworkMode::NETWORK_MODE_HOST,
    NetworkMode::None => pb::NetworkMode::NETWORK_MODE_NONE,
});

enum_map!(semantics_to_pb, semantics_from_pb, VolumeSemantics, pb::VolumeSemantics,
    "a volume semantics", {
    VolumeSemantics::Filesystem => pb::VolumeSemantics::VOLUME_SEMANTICS_FILESYSTEM,
    VolumeSemantics::BlockDevice => pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE,
});

enum_map!(file_access_to_pb, file_access_from_pb, FileAccess, pb::FileAccess, "a file access", {
    FileAccess::HostWorker => pb::FileAccess::FILE_ACCESS_HOST_WORKER,
    FileAccess::GuestWorker => pb::FileAccess::FILE_ACCESS_GUEST_WORKER,
    FileAccess::None => pb::FileAccess::FILE_ACCESS_NONE,
});

fn all<D: Copy, P: buffa::Enumeration>(v: &[D], f: impl Fn(D) -> P) -> Vec<buffa::EnumValue<P>> {
    v.iter().map(|d| f(*d).into()).collect()
}

/// The wire form of amounts per dimension; a dimension absent is zero.
#[must_use]
pub fn resources_to_pb(b: &Bounds) -> pb::Resources {
    let n = |d: Dimension| b.get(&d).copied().unwrap_or(0);
    pb::Resources {
        cpu_millis: n(Dimension::Cpu),
        memory_bytes: n(Dimension::Memory),
        disk_bytes: n(Dimension::Disk),
        disk_read_bytes_per_sec: n(Dimension::DiskReadBytes),
        disk_write_bytes_per_sec: n(Dimension::DiskWriteBytes),
        disk_read_ops_per_sec: n(Dimension::DiskReadOps),
        disk_write_ops_per_sec: n(Dimension::DiskWriteOps),
        bandwidth_bits_per_sec: n(Dimension::Bandwidth),
        workloads: n(Dimension::Workloads),
        volumes: n(Dimension::Volumes),
        endpoints: n(Dimension::Endpoints),
        dedicated_ipv4: n(Dimension::DedicatedIpv4),
        node_ports: n(Dimension::NodePorts),
        devices_gpu: n(Dimension::Devices(DeviceClass::Gpu)),
        devices_usb: n(Dimension::Devices(DeviceClass::Usb)),
        devices_pci: n(Dimension::Devices(DeviceClass::Pci)),
        devices_tpm: n(Dimension::Devices(DeviceClass::Tpm)),
        devices_serial: n(Dimension::Devices(DeviceClass::Serial)),
        ..Default::default()
    }
}

fn capabilities_to_pb(c: &Capabilities) -> pb::Capabilities {
    pb::Capabilities {
        tiers: all(&c.tiers, tier_to_pb),
        console: all(&c.console, console_to_pb),
        exec: c.exec,
        identity_dir: c.identity_dir,
        guest_agent: c.guest_agent,
        signals: c.signals.clone(),
        hot_resize: c.hot_resize.clone(),
        hot_volume_attach: c.hot_volume_attach,
        runtime_snapshot: c
            .runtime_snapshot
            .as_ref()
            .map(|s| pb::SnapshotCaps {
                cpu_template: s.cpu_template.clone(),
                hypervisor_version: s.hypervisor_version.clone(),
                kernel_min: s.kernel_min.clone(),
                kernel_max: s.kernel_max.clone(),
                ..Default::default()
            })
            .into(),
        live_migrate: c.live_migrate,
        device_modes: all(&c.device_modes, mode_to_pb),
        devices: all(&c.devices, device_to_pb),
        rootfs_sources: all(&c.rootfs_sources, rootfs_to_pb),
        network_modes: all(&c.network_modes, network_to_pb),
        enforcement: c
            .enforcement
            .iter()
            .map(|(k, e)| {
                let decl = pb::EnforcementDecl {
                    enforced: e.enforced,
                    mechanism: e.mechanism.clone(),
                    granularity: e.granularity.clone(),
                    ..Default::default()
                };
                (k.clone(), decl)
            })
            .collect(),
        volume_semantics: semantics_to_pb(c.volume_semantics).into(),
        file_access: file_access_to_pb(c.file_access).into(),
        id_mapping: pb::IdMapCaps {
            idmapped_mounts: c.id_mapping.idmapped_mounts,
            user_namespaces: c.id_mapping.user_namespaces,
            ..Default::default()
        }
        .into(),
        config_schema: c.config_schema.clone(),
        disruption: c
            .disruptive
            .iter()
            .map(|(tier, actions)| pb::TierDisruption {
                tier: tier_to_pb(*tier).into(),
                actions: actions.iter().map(|a| a.as_str().to_owned()).collect(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn driver_to_pb(d: &DriverInfo) -> pb::DriverInfo {
    pb::DriverInfo {
        name: d.name.clone(),
        version: d.version.clone(),
        protocol: d.protocol.clone(),
        capabilities: capabilities_to_pb(&d.capabilities).into(),
        healthy: d.healthy,
        ..Default::default()
    }
}

/// The wire form of a node, with what its placements reserve and its place in the mesh.
#[must_use]
pub fn node_to_pb(n: &Node, allocated: &Bounds, overlay: &Standing) -> pb::Node {
    let mut node = pb::Node {
        id: wire_id(IdKind::Node, n.id),
        name: n.name.as_str().to_owned(),
        labels: n.labels.as_map().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        taints: n
            .taints
            .iter()
            .map(|t| pb::Taint { key: t.key.clone(), value: t.value.clone(), ..Default::default() })
            .collect(),
        capacity: resources_to_pb(&n.capacity).into(),
        allocatable: resources_to_pb(&n.allocatable).into(),
        allocated: resources_to_pb(allocated).into(),
        drivers: n.drivers.iter().map(driver_to_pb).collect(),
        status: status_to_pb(n.status()).into(),
        agent_version: n.agent_version.clone().unwrap_or_default(),
        last_seen: n.last_seen.map(timestamp).into(),
        storage_classes: n.storage_classes.iter().map(storage_class_to_pb).collect(),
        reachability: reachability_to_pb(n.reachability).into(),
        organization: wire_id(IdKind::Organization, n.organization),
        admin: admin_to_pb(n.admin).into(),
        connectivity: connectivity_to_pb(n.connectivity).into(),
        addresses: n
            .advertised
            .iter()
            .flat_map(|a| a.addresses.iter().map(ToString::to_string))
            .collect(),
        first_port: n.advertised.as_ref().map_or(0, |a| u32::from(a.ports.first())),
        last_port: n.advertised.as_ref().map_or(0, |a| u32::from(a.ports.last())),
        probed_at: n.probed_at.map(timestamp).into(),
        ingress: n.advertised.as_ref().is_some_and(|a| a.ingress),
        devices: n.devices.iter().map(device_to_wire).collect(),
        unoffered_devices: n.unoffered_devices.iter().map(unoffered_to_wire).collect(),
        cores: n.cores.iter().map(core_to_pb).collect(),
        ..Default::default()
    };
    standing_into(&mut node, overlay);
    node
}

/// What a node says it advertises (§9 of docs/07-networking.md), from the field `field`. A port
/// range of zeros is a node that does not say, and so is no value.
pub fn advertised_from_pb(field: &str, a: &agent::Advertised) -> Result<Option<Advertised>> {
    if a.first_port == 0 && a.last_port == 0 {
        return Ok(None);
    }
    let port = |p: u32| {
        u16::try_from(p).map_err(|_| crate::invalid(field, format!("port {p} is out of range")))
    };
    let ports = PortRange::new(port(a.first_port)?, port(a.last_port)?)
        .map_err(|e| crate::invalid(field, e.to_string()))?;
    let addresses = a
        .addresses
        .iter()
        .map(|s| {
            s.parse::<IpAddr>()
                .map_err(|_| crate::invalid(field, format!("{s:?} is not an IP address")))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(Advertised { addresses, ports, ingress: a.ingress }))
}
