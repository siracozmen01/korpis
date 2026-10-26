// SPDX-License-Identifier: AGPL-3.0-or-later

//! The stored form of an observation's detail: everything but the columns it is keyed and
//! filtered by.

use super::time::{from_time, to_time};
use chrono::{DateTime, Utc};
use korpis_app::ports::StoreError;
use korpis_core::workload::observation::{
    EndpointSeen, Health, Observation, ObservedState, UnsatisfiableReason, Usage, VolumeSeen,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct UsageDoc {
    cpu_millis: u64,
    memory_bytes: u64,
    disk_bytes: u64,
    disk_inodes: u64,
    pids: u64,
    net_rx_bytes: u64,
    net_tx_bytes: u64,
}

#[derive(Serialize, Deserialize)]
struct EndpointDoc {
    name: String,
    address: String,
    ready: bool,
}

#[derive(Serialize, Deserialize)]
struct VolumeDoc {
    name: String,
    volume: String,
    used_bytes: u64,
    used_inodes: u64,
    mounted: bool,
}

/// The detail of an observation.
#[derive(Serialize, Deserialize)]
pub(crate) struct ObservationDoc {
    assignment_seen: u64,
    intent_seen: Option<Uuid>,
    power_seen: u64,
    cycle_seen: u64,
    plan_progress: Option<(Uuid, u32)>,
    reason: Option<String>,
    reason_detail: String,
    health: String,
    exit_code: Option<i32>,
    runtime_id: String,
    usage: UsageDoc,
    endpoints: Vec<EndpointDoc>,
    volumes: Vec<VolumeDoc>,
    since: Option<DateTime<Utc>>,
    /// Absent from observations recorded before addresses were.
    #[serde(default)]
    address: Option<std::net::Ipv4Addr>,
    /// Absent from observations recorded before IPv6 addresses were.
    #[serde(default)]
    address_ipv6: Option<std::net::Ipv6Addr>,
}

pub(crate) fn state_str(s: ObservedState) -> &'static str {
    match s {
        ObservedState::Running => "running",
        ObservedState::Stopped => "stopped",
        ObservedState::Starting => "starting",
        ObservedState::Stopping => "stopping",
        ObservedState::Crashed => "crashed",
        ObservedState::Unsatisfiable => "unsatisfiable",
    }
}

fn state(s: &str) -> Option<ObservedState> {
    [
        ObservedState::Running,
        ObservedState::Stopped,
        ObservedState::Starting,
        ObservedState::Stopping,
        ObservedState::Crashed,
        ObservedState::Unsatisfiable,
    ]
    .into_iter()
    .find(|x| state_str(*x) == s)
}

const REASONS: [(UnsatisfiableReason, &str); 6] = [
    (UnsatisfiableReason::MissingDriver, "missing_driver"),
    (UnsatisfiableReason::InsufficientResources, "insufficient_resources"),
    (UnsatisfiableReason::VolumeUnavailable, "volume_unavailable"),
    (UnsatisfiableReason::DeviceUnavailable, "device_unavailable"),
    (UnsatisfiableReason::ImageUnsupported, "image_unsupported"),
    (UnsatisfiableReason::PrepareRejected, "prepare_rejected"),
];

const HEALTH: [(Health, &str); 3] =
    [(Health::Healthy, "healthy"), (Health::Unhealthy, "unhealthy"), (Health::Unknown, "unknown")];

fn named<T: Copy + PartialEq>(table: &[(T, &'static str)], v: T) -> &'static str {
    table.iter().find(|(x, _)| *x == v).map_or("", |(_, n)| n)
}

fn parsed<T: Copy>(table: &[(T, &'static str)], s: &str) -> Option<T> {
    table.iter().find(|(_, n)| *n == s).map(|(x, _)| *x)
}

impl ObservationDoc {
    /// The detail of `o`.
    pub(crate) fn from_domain(o: &Observation) -> Self {
        let u = o.usage;
        Self {
            assignment_seen: o.assignment_seen,
            intent_seen: o.intent_seen,
            power_seen: o.power_seen,
            cycle_seen: o.cycle_seen,
            plan_progress: o.plan_progress,
            reason: o.reason.map(|r| named(&REASONS, r).to_owned()),
            reason_detail: o.reason_detail.clone(),
            health: named(&HEALTH, o.health).to_owned(),
            exit_code: o.exit_code,
            runtime_id: o.runtime_id.clone(),
            usage: UsageDoc {
                cpu_millis: u.cpu_millis,
                memory_bytes: u.memory_bytes,
                disk_bytes: u.disk_bytes,
                disk_inodes: u.disk_inodes,
                pids: u.pids,
                net_rx_bytes: u.net_rx_bytes,
                net_tx_bytes: u.net_tx_bytes,
            },
            endpoints: o
                .endpoints
                .iter()
                .map(|e| EndpointDoc {
                    name: e.name.clone(),
                    address: e.address.clone(),
                    ready: e.ready,
                })
                .collect(),
            volumes: o
                .volumes
                .iter()
                .map(|v| VolumeDoc {
                    name: v.name.clone(),
                    volume: v.volume.clone(),
                    used_bytes: v.used_bytes,
                    used_inodes: v.used_inodes,
                    mounted: v.mounted,
                })
                .collect(),
            since: o.since.map(from_time),
            address: o.address,
            address_ipv6: o.address_ipv6,
        }
    }

    /// The observation with this detail and the row's columns.
    pub(crate) fn into_domain(
        self,
        workload: Uuid,
        node: Uuid,
        observed_at: DateTime<Utc>,
        state_name: &str,
    ) -> Result<Observation, StoreError> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("observation {what}"));
        let reason = match self.reason {
            Some(r) => Some(parsed(&REASONS, &r).ok_or_else(|| corrupt("reason"))?),
            None => None,
        };
        let u = self.usage;
        Ok(Observation {
            workload,
            node,
            observed_at: to_time(observed_at),
            assignment_seen: self.assignment_seen,
            intent_seen: self.intent_seen,
            power_seen: self.power_seen,
            cycle_seen: self.cycle_seen,
            plan_progress: self.plan_progress,
            state: state(state_name).ok_or_else(|| corrupt("state"))?,
            reason,
            reason_detail: self.reason_detail,
            health: parsed(&HEALTH, &self.health).ok_or_else(|| corrupt("health"))?,
            exit_code: self.exit_code,
            runtime_id: self.runtime_id,
            usage: Usage {
                cpu_millis: u.cpu_millis,
                memory_bytes: u.memory_bytes,
                disk_bytes: u.disk_bytes,
                disk_inodes: u.disk_inodes,
                pids: u.pids,
                net_rx_bytes: u.net_rx_bytes,
                net_tx_bytes: u.net_tx_bytes,
            },
            endpoints: self
                .endpoints
                .into_iter()
                .map(|e| EndpointSeen { name: e.name, address: e.address, ready: e.ready })
                .collect(),
            volumes: self
                .volumes
                .into_iter()
                .map(|v| VolumeSeen {
                    name: v.name,
                    volume: v.volume,
                    used_bytes: v.used_bytes,
                    used_inodes: v.used_inodes,
                    mounted: v.mounted,
                })
                .collect(),
            since: self.since.map(to_time),
            address: self.address,
            address_ipv6: self.address_ipv6,
        })
    }
}
