// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an agent reports about its node (§4.2 of docs/02-architecture.md): capacity, drivers
//! with their declared capabilities, and storage classes. Read strictly: an unknown enum value
//! is refused rather than read as some default a scheduler would then trust.

use super::{
    device_from_pb, file_access_from_pb, gpu_from_pb, network_from_pb, rootfs_from_pb,
    semantics_from_pb,
};
use crate::spec::{console_from_pb, tier_from_pb};
use crate::{Result, invalid};
use korpis_core::node::driver::{IdMapCaps, SnapshotCaps};
use korpis_core::node::{Capabilities, DriverInfo, Enforcement};
use korpis_core::tenancy::quota::{Bounds, Dimension};
use korpis_core::workload::plan::StepAction;
use korpis_core::workload::spec::IsolationTier;
use korpis_proto::korpis::v1 as pb;

/// Amounts per dimension as reported; a zero is absent.
#[must_use]
pub fn resources_from_pb(r: &pb::Resources) -> Bounds {
    [
        (Dimension::Cpu, r.cpu_millis),
        (Dimension::Memory, r.memory_bytes),
        (Dimension::Disk, r.disk_bytes),
        (Dimension::DiskReadBytes, r.disk_read_bytes_per_sec),
        (Dimension::DiskWriteBytes, r.disk_write_bytes_per_sec),
        (Dimension::DiskReadOps, r.disk_read_ops_per_sec),
        (Dimension::DiskWriteOps, r.disk_write_ops_per_sec),
        (Dimension::Bandwidth, r.bandwidth_bits_per_sec),
        (Dimension::Workloads, r.workloads),
        (Dimension::Volumes, r.volumes),
        (Dimension::Endpoints, r.endpoints),
        (Dimension::DedicatedIpv4, r.dedicated_ipv4),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .collect()
}

fn each<P: buffa::Enumeration + Copy, D>(
    field: &str,
    v: &[buffa::EnumValue<P>],
    f: impl Fn(&str, buffa::EnumValue<P>) -> Result<D>,
) -> Result<Vec<D>> {
    v.iter().enumerate().map(|(i, e)| f(&format!("{field}[{i}]"), *e)).collect()
}

fn capabilities_from_pb(field: &str, c: &pb::Capabilities) -> Result<Capabilities> {
    let f = |name: &str| format!("{field}.{name}");
    Ok(Capabilities {
        tiers: each(&f("tiers"), &c.tiers, tier_from_pb)?,
        console: each(&f("console"), &c.console, console_from_pb)?,
        exec: c.exec,
        signals: c.signals.clone(),
        hot_resize: c.hot_resize.clone(),
        hot_volume_attach: c.hot_volume_attach,
        runtime_snapshot: c.runtime_snapshot.as_option().map(|s| SnapshotCaps {
            cpu_template: s.cpu_template.clone(),
            hypervisor_version: s.hypervisor_version.clone(),
            kernel_min: s.kernel_min.clone(),
            kernel_max: s.kernel_max.clone(),
        }),
        live_migrate: c.live_migrate,
        gpu: each(&f("gpu"), &c.gpu, gpu_from_pb)?,
        devices: each(&f("devices"), &c.devices, device_from_pb)?,
        rootfs_sources: each(&f("rootfs_sources"), &c.rootfs_sources, rootfs_from_pb)?,
        network_modes: each(&f("network_modes"), &c.network_modes, network_from_pb)?,
        enforcement: c
            .enforcement
            .iter()
            .map(|(k, e)| {
                let decl = Enforcement {
                    enforced: e.enforced,
                    mechanism: e.mechanism.clone(),
                    granularity: e.granularity.clone(),
                };
                (k.clone(), decl)
            })
            .collect(),
        volume_semantics: semantics_from_pb(&f("volume_semantics"), c.volume_semantics)?,
        file_access: file_access_from_pb(&f("file_access"), c.file_access)?,
        id_mapping: c.id_mapping.as_option().map_or(
            IdMapCaps { idmapped_mounts: false, user_namespaces: false },
            |m| IdMapCaps {
                idmapped_mounts: m.idmapped_mounts,
                user_namespaces: m.user_namespaces,
            },
        ),
        config_schema: c.config_schema.clone(),
        disruptive: disruption_from_pb(&f("disruption"), &c.disruption)?,
    })
}

/// A driver's classification layer. An action name outside the vocabulary is refused: the layer
/// only makes steps more disruptive, so one silently dropped would make a step less so than its
/// driver declared.
fn disruption_from_pb(
    field: &str,
    d: &[pb::TierDisruption],
) -> Result<Vec<(IsolationTier, Vec<StepAction>)>> {
    d.iter()
        .enumerate()
        .map(|(i, t)| {
            let at = format!("{field}[{i}]");
            let tier = tier_from_pb(&format!("{at}.tier"), t.tier)?;
            let actions = t
                .actions
                .iter()
                .map(|a| {
                    StepAction::parse(a)
                        .ok_or_else(|| invalid(&format!("{at}.actions"), "not a step action"))
                })
                .collect::<Result<_>>()?;
            Ok((tier, actions))
        })
        .collect()
}

/// The drivers an agent reports, each with its capabilities.
pub fn drivers_from_pb(field: &str, drivers: &[pb::DriverInfo]) -> Result<Vec<DriverInfo>> {
    drivers
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let at = format!("{field}[{i}]");
            if d.name.is_empty() {
                return Err(invalid(&format!("{at}.name"), "required"));
            }
            let caps = d
                .capabilities
                .as_option()
                .ok_or_else(|| invalid(&format!("{at}.capabilities"), "required"))?;
            Ok(DriverInfo {
                name: d.name.clone(),
                version: d.version.clone(),
                protocol: d.protocol.clone(),
                healthy: d.healthy,
                capabilities: capabilities_from_pb(&format!("{at}.capabilities"), caps)?,
            })
        })
        .collect()
}
