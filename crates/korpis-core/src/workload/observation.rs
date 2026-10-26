// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node reports about one workload (§4.3 of docs/02-architecture.md): field by field,
//! what it has converged to, and what it sees. The latest replaces the one before: it is "now",
//! not history, and history is the effect log.

use std::time::SystemTime;
use uuid::Uuid;

/// The runtime's state as the node sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedState {
    /// Running.
    Running,
    /// Stopped.
    Stopped,
    /// Being started.
    Starting,
    /// Being stopped.
    Stopping,
    /// Exited when it should be running.
    Crashed,
    /// The node holds the assignment and cannot satisfy it.
    Unsatisfiable,
}

/// Why a node cannot satisfy an assignment (§7 of docs/05-scheduling.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsatisfiableReason {
    /// The driver the intent needs is not on the node.
    MissingDriver,
    /// It does not fit.
    InsufficientResources,
    /// A volume cannot be attached.
    VolumeUnavailable,
    /// A device is not available.
    DeviceUnavailable,
    /// The image is not one the driver runs.
    ImageUnsupported,
    /// The driver's `Prepare` refused it.
    PrepareRejected,
}

/// The application's health, as its probe judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Healthy.
    Healthy,
    /// Unhealthy.
    Unhealthy,
    /// Not judged.
    Unknown,
}

impl Health {
    /// Its name in events and documents.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
        }
    }
}

/// Resources in use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// CPU, in millicores.
    pub cpu_millis: u64,
    /// Memory.
    pub memory_bytes: u64,
    /// Disk.
    pub disk_bytes: u64,
    /// Inodes.
    pub disk_inodes: u64,
    /// Processes.
    pub pids: u64,
    /// Received.
    pub net_rx_bytes: u64,
    /// Sent.
    pub net_tx_bytes: u64,
}

/// One endpoint as the node sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSeen {
    /// Its name in the intent.
    pub name: String,
    /// Where it answers.
    pub address: String,
    /// Whether it answers.
    pub ready: bool,
}

/// One volume as the node sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSeen {
    /// Its name in the intent.
    pub name: String,
    /// The volume.
    pub volume: String,
    /// Bytes used.
    pub used_bytes: u64,
    /// Inodes used.
    pub used_inodes: u64,
    /// Whether it is mounted.
    pub mounted: bool,
}

/// A node's latest report about a workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The workload.
    pub workload: Uuid,
    /// The node.
    pub node: Uuid,
    /// The node's clock when it observed.
    pub observed_at: SystemTime,
    /// The assignment generation it has applied.
    pub assignment_seen: u64,
    /// The target intent it has converged to.
    pub intent_seen: Option<Uuid>,
    /// The power declaration version it has converged to.
    pub power_seen: u64,
    /// The power cycle it last performed.
    pub cycle_seen: u64,
    /// The last plan step it completed: the plan and the step.
    pub plan_progress: Option<(Uuid, u32)>,
    /// The runtime's state.
    pub state: ObservedState,
    /// For `Unsatisfiable`: why.
    pub reason: Option<UnsatisfiableReason>,
    /// For `Unsatisfiable`: in words.
    pub reason_detail: String,
    /// The application's health.
    pub health: Health,
    /// The last exit code, if it exited.
    pub exit_code: Option<i32>,
    /// The runtime's own identifier.
    pub runtime_id: String,
    /// Resources in use.
    pub usage: Usage,
    /// Its endpoints.
    pub endpoints: Vec<EndpointSeen>,
    /// Its volumes.
    pub volumes: Vec<VolumeSeen>,
    /// When the state last changed, by the node's clock: exact however late the report.
    pub since: Option<SystemTime>,
    /// Its IPv4 address on its node, which the node gave it (§4.1 of docs/07-networking.md);
    /// none where it has no network.
    pub address: Option<std::net::Ipv4Addr>,
    /// Its IPv6 address on its node, where the node gave it one, which a rule naming workloads
    /// names of it (§6 of docs/07-networking.md).
    pub address_ipv6: Option<std::net::Ipv6Addr>,
}
