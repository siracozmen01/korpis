// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resources and volumes.

use crate::{Result, id, invalid, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::spec::resources::{
    BandwidthSpec, CpuIsolation, CpuSpec, IoSpec, ReservedLimit, ResourceSpec,
};
use korpis_core::workload::spec::volumes::{VolumeMode, VolumeSpec};
use korpis_proto::korpis::v1 as pb;

fn rl_to_pb(r: ReservedLimit) -> pb::ReservedLimit {
    pb::ReservedLimit { reservation: r.reservation, limit: r.limit, ..Default::default() }
}

fn rl(r: Option<&pb::ReservedLimit>) -> ReservedLimit {
    r.map_or_else(ReservedLimit::default, |r| ReservedLimit {
        reservation: r.reservation,
        limit: r.limit,
    })
}

fn isolation(field: &str, v: buffa::EnumValue<pb::CpuIsolation>) -> Result<CpuIsolation> {
    match v.as_known() {
        Some(
            pb::CpuIsolation::CPU_ISOLATION_UNSPECIFIED | pb::CpuIsolation::CPU_ISOLATION_SHARED,
        ) => Ok(CpuIsolation::Shared),
        Some(pb::CpuIsolation::CPU_ISOLATION_DEDICATED_CORES) => Ok(CpuIsolation::DedicatedCores),
        None => Err(invalid(field, "unknown CPU isolation")),
    }
}

/// The wire form of a resource declaration.
#[must_use]
pub fn resources_to_pb(r: &ResourceSpec) -> pb::ResourceSpec {
    pb::ResourceSpec {
        cpu: pb::CpuSpec {
            reservation_millis: r.cpu.reservation_millis,
            limit_millis: r.cpu.limit_millis,
            isolation: match r.cpu.isolation {
                CpuIsolation::Shared => pb::CpuIsolation::CPU_ISOLATION_SHARED,
                CpuIsolation::DedicatedCores => pb::CpuIsolation::CPU_ISOLATION_DEDICATED_CORES,
            }
            .into(),
            ..Default::default()
        }
        .into(),
        memory: rl_to_pb(r.memory).into(),
        disk: rl_to_pb(r.disk).into(),
        io: pb::IoSpec {
            read_bytes_per_sec: r.io.read_bytes_per_sec,
            write_bytes_per_sec: r.io.write_bytes_per_sec,
            read_ops_per_sec: r.io.read_ops_per_sec,
            write_ops_per_sec: r.io.write_ops_per_sec,
            share: r.io.share,
            ..Default::default()
        }
        .into(),
        bandwidth: pb::BandwidthSpec {
            egress: rl_to_pb(r.bandwidth.egress).into(),
            ingress_limit: r.bandwidth.ingress_limit,
            ..Default::default()
        }
        .into(),
        pids: pb::PidsSpec { limit: r.pids_limit, ..Default::default() }.into(),
        ..Default::default()
    }
}

/// A resource declaration from the field `field`; absent parts are unset.
pub fn resources_from_pb(field: &str, r: Option<&pb::ResourceSpec>) -> Result<ResourceSpec> {
    let Some(r) = r else { return Ok(ResourceSpec::default()) };
    let cpu = r.cpu.as_option();
    let io = r.io.as_option();
    let bw = r.bandwidth.as_option();
    Ok(ResourceSpec {
        cpu: CpuSpec {
            reservation_millis: cpu.map_or(0, |c| c.reservation_millis),
            limit_millis: cpu.map_or(0, |c| c.limit_millis),
            isolation: match cpu {
                Some(c) => isolation(&format!("{field}.cpu.isolation"), c.isolation)?,
                None => CpuIsolation::Shared,
            },
        },
        memory: rl(r.memory.as_option()),
        disk: rl(r.disk.as_option()),
        io: io.map_or_else(IoSpec::default, |io| IoSpec {
            read_bytes_per_sec: io.read_bytes_per_sec,
            write_bytes_per_sec: io.write_bytes_per_sec,
            read_ops_per_sec: io.read_ops_per_sec,
            write_ops_per_sec: io.write_ops_per_sec,
            share: io.share,
        }),
        bandwidth: bw.map_or_else(BandwidthSpec::default, |b| BandwidthSpec {
            egress: rl(b.egress.as_option()),
            ingress_limit: b.ingress_limit,
        }),
        pids_limit: r.pids.as_option().map_or(0, |p| p.limit),
    })
}

pub(crate) fn mode(field: &str, v: buffa::EnumValue<pb::VolumeMode>) -> Result<VolumeMode> {
    match v.as_known() {
        Some(pb::VolumeMode::VOLUME_MODE_UNSPECIFIED | pb::VolumeMode::VOLUME_MODE_RW) => {
            Ok(VolumeMode::ReadWrite)
        }
        Some(pb::VolumeMode::VOLUME_MODE_RO) => Ok(VolumeMode::ReadOnly),
        Some(pb::VolumeMode::VOLUME_MODE_SHARED) => Ok(VolumeMode::Shared),
        None => Err(invalid(field, "unknown volume mode")),
    }
}

/// The wire form of a volume declaration.
#[must_use]
pub fn volume_to_pb(v: &VolumeSpec) -> pb::VolumeSpec {
    pb::VolumeSpec {
        name: v.name.clone(),
        path: v.path.clone(),
        size_bytes: v.size_bytes,
        storage_class: v.storage_class.clone(),
        mode: match v.mode {
            VolumeMode::ReadWrite => pb::VolumeMode::VOLUME_MODE_RW,
            VolumeMode::ReadOnly => pb::VolumeMode::VOLUME_MODE_RO,
            VolumeMode::Shared => pb::VolumeMode::VOLUME_MODE_SHARED,
        }
        .into(),
        existing: v.existing.map(|e| wire_id(IdKind::Volume, e)),
        ..Default::default()
    }
}

/// A volume declaration from the field `field`.
pub fn volume_from_pb(field: &str, v: &pb::VolumeSpec) -> Result<VolumeSpec> {
    Ok(VolumeSpec {
        name: v.name.clone(),
        path: v.path.clone(),
        size_bytes: v.size_bytes,
        storage_class: v.storage_class.clone(),
        mode: mode(&format!("{field}.mode"), v.mode)?,
        existing: v
            .existing
            .as_deref()
            .map(|e| id(IdKind::Volume, &format!("{field}.existing"), e))
            .transpose()?,
    })
}
