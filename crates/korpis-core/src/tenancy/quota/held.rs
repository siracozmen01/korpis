// SPDX-License-Identifier: AGPL-3.0-or-later

//! What one workload holds against quota (§5.1 of docs/05-scheduling.md), from its declaration.

use super::Dimension;
use crate::workload::spec::endpoints::Exposure;
use crate::workload::spec::resources::ReservedLimit;
use crate::workload::spec::{ConsoleMode, IntentBody};
use std::collections::BTreeMap;

/// What a workload holds against quota.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Held {
    /// Per dimension, reserved.
    pub reservations: BTreeMap<Dimension, u64>,
    /// Per dimension, the limit; `None` where the workload sets none, which no finite bound
    /// can hold.
    pub limits: BTreeMap<Dimension, Option<u64>>,
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// What the replicas of `body`'s volumes hold on other nodes: each volume's size once for each
/// replica it asks for.
#[must_use]
pub fn replica_bytes(body: &IntentBody) -> u64 {
    body.volumes
        .iter()
        .filter(|v| v.existing.is_none())
        .filter_map(|v| v.replication.map(|r| v.size_bytes.saturating_mul(u64::from(r.replicas))))
        .fold(0, u64::saturating_add)
}

impl Held {
    /// What a workload declared by `body` holds.
    #[must_use]
    pub fn of(body: &IntentBody) -> Self {
        use Dimension as D;
        let r = &body.resources;
        let created: Vec<_> = body.volumes.iter().filter(|v| v.existing.is_none()).collect();
        let volume_bytes = created.iter().fold(0u64, |a, v| a.saturating_add(v.size_bytes));
        // A console stream holds disk like a volume it creates (§4 of docs/14-streams.md), and
        // a workload whose runs write their own holds one for each run its node keeps at once:
        // those retention keeps and those active, which its policy bounds.
        let streams = match (body.console, body.lifecycle.has_runs()) {
            (ConsoleMode::None, _) => 0,
            (_, false) => 1,
            (_, true) => u64::from(body.run_policy().streams_held()),
        };
        let stream_bytes = body.streams.disk_bound().saturating_mul(streams);
        // Its volumes' replicas hold as much again on other nodes, each (§2.2 of
        // docs/06-storage.md): counted here, against quota, and not in what its own node
        // reserves (`schedule::reservation`).
        let created_bytes =
            volume_bytes.saturating_add(stream_bytes).saturating_add(replica_bytes(body));
        // A workload's `dedicated_ip` endpoints share its one address (§3.7 of
        // docs/07-networking.md, Finding 185 of docs/23-walkthroughs.md).
        let dedicated = body.endpoints.iter().any(|e| e.exposure == Exposure::DedicatedIp);
        let counts = [
            (D::Workloads, 1),
            (D::Volumes, count(created.len())),
            (D::Endpoints, count(body.endpoints.len())),
            (D::DedicatedIpv4, u64::from(dedicated)),
            // Counted as declared, a `stable` endpoint as a port of its node whatever serves
            // it: admission comes before the placement that resolves it, and a quota's totals do
            // not move when an edge group is made or loses a member. What its node reserves
            // knows the edge (`schedule::reservation`, §3.1 of docs/07-networking.md).
            (D::NodePorts, crate::workload::endpoint::node_ports(&body.endpoints, false)),
        ];
        let devices = body.devices.iter().map(|d| (D::Devices(d.class), u64::from(d.count)));
        // A limit left unset is the reservation (`ReservedLimit::effective`); the I/O ceilings
        // have no reservation, so unset there is unbounded.
        let ceiling = |n: u64| (n > 0).then_some(n);
        let cpu =
            ReservedLimit { reservation: r.cpu.reservation_millis, limit: r.cpu.limit_millis };
        let mut reservations = BTreeMap::from([
            (D::Cpu, r.cpu.reservation_millis),
            (D::Memory, r.memory.reservation),
            (D::Disk, r.disk.reservation.saturating_add(created_bytes)),
            (D::Bandwidth, r.bandwidth.egress.reservation),
        ]);
        let mut limits = BTreeMap::from([
            (D::Cpu, Some(cpu.effective())),
            (D::Memory, Some(r.memory.effective())),
            (D::Disk, Some(r.disk.effective().saturating_add(created_bytes))),
            (D::DiskReadBytes, ceiling(r.io.read_bytes_per_sec)),
            (D::DiskWriteBytes, ceiling(r.io.write_bytes_per_sec)),
            (D::DiskReadOps, ceiling(r.io.read_ops_per_sec)),
            (D::DiskWriteOps, ceiling(r.io.write_ops_per_sec)),
            (D::Bandwidth, Some(r.bandwidth.egress.effective())),
        ]);
        for (d, n) in counts {
            reservations.insert(d, n);
            limits.insert(d, Some(n));
        }
        // Requests of one class add up.
        for (d, n) in devices {
            let held = reservations.entry(d).or_insert(0);
            *held = held.saturating_add(n);
            limits.insert(d, Some(*held));
        }
        Self { reservations, limits }
    }

    /// The larger of two holdings, per dimension; an unbounded limit is the larger.
    #[must_use]
    pub fn max(&self, other: &Self) -> Self {
        let mut out = self.clone();
        for (d, n) in &other.reservations {
            let slot = out.reservations.entry(*d).or_insert(0);
            *slot = (*slot).max(*n);
        }
        for (d, n) in &other.limits {
            let slot = out.limits.entry(*d).or_insert(Some(0));
            *slot = match (*slot, *n) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
        }
        out
    }
}
