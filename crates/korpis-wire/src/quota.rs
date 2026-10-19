// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota sets on the wire (§5.1 of docs/05-scheduling.md): bounds with explicit presence, so a
//! bound of zero allows none of a dimension; an entry's shape by its two selectors; and what the
//! workloads it counts hold.

use crate::scope::{selector_from_pb, selector_to_pb};
use crate::{Result, invalid};
use korpis_core::tenancy::quota::{
    Bounds, Dimension as D, QuotaEntry, QuotaOwner, QuotaSet, Totals,
};
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

fn slots(b: &pb::QuotaBounds) -> [(D, Option<u64>); 13] {
    [
        (D::Cpu, b.cpu_millis),
        (D::Memory, b.memory_bytes),
        (D::Disk, b.disk_bytes),
        (D::DiskReadBytes, b.disk_read_bytes_per_sec),
        (D::DiskWriteBytes, b.disk_write_bytes_per_sec),
        (D::DiskReadOps, b.disk_read_ops_per_sec),
        (D::DiskWriteOps, b.disk_write_ops_per_sec),
        (D::Bandwidth, b.bandwidth_bits_per_sec),
        (D::Workloads, b.workloads),
        (D::Volumes, b.volumes),
        (D::Endpoints, b.endpoints),
        (D::DedicatedIpv4, b.dedicated_ipv4),
        (D::NodePorts, b.node_ports),
    ]
}

/// The bounds `b` gives; an absent message bounds nothing.
#[must_use]
pub fn bounds_from_pb(b: Option<&pb::QuotaBounds>) -> Bounds {
    b.map(|b| slots(b).into_iter().filter_map(|(d, n)| n.map(|n| (d, n))).collect())
        .unwrap_or_default()
}

/// `b` on the wire.
#[must_use]
pub fn bounds_to_pb(b: &Bounds) -> pb::QuotaBounds {
    let get = |d: D| b.get(&d).copied();
    pb::QuotaBounds {
        cpu_millis: get(D::Cpu),
        memory_bytes: get(D::Memory),
        disk_bytes: get(D::Disk),
        disk_read_bytes_per_sec: get(D::DiskReadBytes),
        disk_write_bytes_per_sec: get(D::DiskWriteBytes),
        disk_read_ops_per_sec: get(D::DiskReadOps),
        disk_write_ops_per_sec: get(D::DiskWriteOps),
        bandwidth_bits_per_sec: get(D::Bandwidth),
        workloads: get(D::Workloads),
        volumes: get(D::Volumes),
        endpoints: get(D::Endpoints),
        dedicated_ipv4: get(D::DedicatedIpv4),
        node_ports: get(D::NodePorts),
        ..Default::default()
    }
}

fn held_to_pb(t: &Totals) -> pb::QuotaHeld {
    pb::QuotaHeld {
        reservations: bounds_to_pb(&t.reservations).into(),
        limits: bounds_to_pb(&t.limits).into(),
        unbounded: t.unbounded.keys().map(|d| d.as_str().to_owned()).collect(),
        ..Default::default()
    }
}

fn entry_to_pb(e: &QuotaEntry) -> pb::QuotaEntry {
    pb::QuotaEntry {
        selector: e.selector.as_ref().map(selector_to_pb).into(),
        version: e.version,
        nodes: e.nodes.as_ref().map(selector_to_pb).into(),
        reservation_bounds: bounds_to_pb(&e.reservations).into(),
        limit_bounds: bounds_to_pb(&e.limits).into(),
        held: e.held.as_ref().map(held_to_pb).into(),
        ..Default::default()
    }
}

/// `s` on the wire.
#[must_use]
pub fn quota_set_to_pb(s: &QuotaSet) -> pb::QuotaSet {
    pb::QuotaSet {
        entries: s.entries.iter().map(entry_to_pb).collect(),
        child_overcommit_percent: s
            .child_overcommit
            .iter()
            .map(|(d, p)| (d.as_str().to_owned(), *p))
            .collect(),
        ..Default::default()
    }
}

fn entry_from_pb(field: &str, owner: QuotaOwner, e: &pb::QuotaEntry) -> Result<QuotaEntry> {
    if e.egress_exhausted.is_set() {
        let why = "shaping at an exhausted egress budget is not built yet (§7 of \
                   docs/07-networking.md): leave it unset";
        return Err(invalid(&format!("{field}.egress_exhausted"), why));
    }
    let selector = |name: &str, s: Option<&pb::LabelSelector>| {
        s.map(|s| selector_from_pb(&format!("{field}.{name}"), Some(s))).transpose()
    };
    Ok(QuotaEntry {
        id: Uuid::nil(),
        owner,
        selector: selector("selector", e.selector.as_option())?,
        nodes: selector("nodes", e.nodes.as_option())?,
        reservations: bounds_from_pb(e.reservation_bounds.as_option()),
        limits: bounds_from_pb(e.limit_bounds.as_option()),
        version: 0,
        held: None,
    })
}

/// The set `s` of `owner`, from the field `field`: its entries are new until they are stored,
/// and what they hold is never taken from a caller.
pub fn quota_set_from_pb(field: &str, owner: QuotaOwner, s: &pb::QuotaSet) -> Result<QuotaSet> {
    let entries = s
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| entry_from_pb(&format!("{field}.entries[{i}]"), owner, e))
        .collect::<Result<Vec<_>>>()?;
    let child_overcommit = s
        .child_overcommit_percent
        .iter()
        .map(|(name, percent)| {
            D::parse(name).map(|d| (d, *percent)).ok_or_else(|| {
                invalid(
                    &format!("{field}.child_overcommit_percent"),
                    format!("no dimension {name:?}"),
                )
            })
        })
        .collect::<Result<_>>()?;
    Ok(QuotaSet { entries, child_overcommit })
}
