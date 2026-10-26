// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota (§5.1 of docs/05-scheduling.md): selector-scoped entries, each with reservations and
//! limits, held at admission in the transaction that would exceed them.
//!
//! What a workload holds is computed from its intents, never kept as a running counter that
//! could drift from them. It is the larger, per dimension, of what its head declares (which may
//! be approved at any moment) and what its target runs (which the kernel grants now): holding
//! either one alone would let a workload awaiting approval, or one being shrunk, hold less than
//! it may use (§5.1, "Quota holds the enforced value, not the declared one"). What they hold
//! together is kept beside each entry as its totals, so admission reads one row per entry.

mod egress;
mod promised;
mod set;
mod totals;

pub use egress::{BudgetError, EgressBudget, Exhaustion, Spent, month_of};
pub use promised::{Promise, keeps_promises};
pub use set::{EXCLUSIVE, QuotaSet, SetError, Shape, shape};
pub use totals::Totals;

use crate::id::{Id, IdKind};
use crate::labels::LabelSelector;
use crate::node::driver::DeviceClass;
use crate::workload::spec::endpoints::Exposure;
use crate::workload::spec::resources::ReservedLimit;
use crate::workload::spec::{ConsoleMode, IntentBody};
use std::collections::BTreeMap;
use std::fmt;
use uuid::Uuid;

/// A dimension admission can hold (§3.1 of docs/01-model.md). Egress is metered per period and
/// shaped, not admitted (§3.1), so it is not one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dimension {
    /// Millicores.
    Cpu,
    /// Bytes.
    Memory,
    /// Bytes, including every volume the workload creates.
    Disk,
    /// Bytes read per second; a limit only.
    DiskReadBytes,
    /// Bytes written per second; a limit only.
    DiskWriteBytes,
    /// Read operations per second; a limit only.
    DiskReadOps,
    /// Write operations per second; a limit only.
    DiskWriteOps,
    /// Egress bits per second.
    Bandwidth,
    /// Workloads.
    Workloads,
    /// Volumes created.
    Volumes,
    /// Endpoints.
    Endpoints,
    /// Endpoints with a dedicated IPv4 address; never overcommitted.
    DedicatedIpv4,
    /// Ports of a node held by endpoints the node itself exposes, `node_port` and `node_bound`
    /// (§3 of docs/07-networking.md); never overcommitted, since a port is one endpoint's.
    NodePorts,
    /// Devices held, one each, by class (§6.4 of docs/05-scheduling.md); never overcommitted.
    Devices(DeviceClass),
}

impl Dimension {
    /// Every dimension.
    pub const ALL: [Self; 18] = [
        Self::Cpu,
        Self::Memory,
        Self::Disk,
        Self::DiskReadBytes,
        Self::DiskWriteBytes,
        Self::DiskReadOps,
        Self::DiskWriteOps,
        Self::Bandwidth,
        Self::Workloads,
        Self::Volumes,
        Self::Endpoints,
        Self::DedicatedIpv4,
        Self::NodePorts,
        Self::Devices(DeviceClass::Gpu),
        Self::Devices(DeviceClass::Usb),
        Self::Devices(DeviceClass::Pci),
        Self::Devices(DeviceClass::Tpm),
        Self::Devices(DeviceClass::Serial),
    ];

    /// The stored and wire name: the field of `korpis.v1.Resources`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu_millis",
            Self::Memory => "memory_bytes",
            Self::Disk => "disk_bytes",
            Self::DiskReadBytes => "disk_read_bytes_per_sec",
            Self::DiskWriteBytes => "disk_write_bytes_per_sec",
            Self::DiskReadOps => "disk_read_ops_per_sec",
            Self::DiskWriteOps => "disk_write_ops_per_sec",
            Self::Bandwidth => "bandwidth_bits_per_sec",
            Self::Workloads => "workloads",
            Self::Volumes => "volumes",
            Self::Endpoints => "endpoints",
            Self::DedicatedIpv4 => "dedicated_ipv4",
            Self::NodePorts => "node_ports",
            Self::Devices(DeviceClass::Gpu) => "devices_gpu",
            Self::Devices(DeviceClass::Usb) => "devices_usb",
            Self::Devices(DeviceClass::Pci) => "devices_pci",
            Self::Devices(DeviceClass::Tpm) => "devices_tpm",
            Self::Devices(DeviceClass::Serial) => "devices_serial",
        }
    }

    /// The dimension a name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == s)
    }
}

/// Bounds per dimension; a dimension absent is not bounded.
pub type Bounds = BTreeMap<Dimension, u64>;

/// Whose quota an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaOwner {
    /// An organization: it bounds everything below it, sub-organizations included.
    Organization(Uuid),
    /// A project.
    Project(Uuid),
}

impl fmt::Display for QuotaOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Organization(id) => {
                write!(f, "organization {}", Id::from_uuid(IdKind::Organization, *id))
            }
            Self::Project(id) => write!(f, "project {}", Id::from_uuid(IdKind::Project, *id)),
        }
    }
}

/// One entry of a quota set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaEntry {
    /// Its identity.
    pub id: Uuid,
    /// Whose it is.
    pub owner: QuotaOwner,
    /// The workloads it counts, by their labels; `None` counts every workload of the owner.
    pub selector: Option<LabelSelector>,
    /// The nodes it counts them on, by their labels: such an entry counts a workload while it is
    /// placed on one of them, by what its placement runs, and is checked when it is bound (§5.1,
    /// "Where a workload runs"). `None` counts it wherever it is, from its declaration.
    pub nodes: Option<LabelSelector>,
    /// Guaranteed capacity: what the reservations of the workloads it counts may sum to.
    pub reservations: Bounds,
    /// Sold capacity: what their limits may sum to.
    pub limits: Bounds,
    /// Participates in the admission compare-and-swap.
    pub version: u64,
    /// Its egress budget for each month, and what a spent one does; none for no budget.
    pub egress: Option<EgressBudget>,
    /// What the workloads it counts hold together; `None` until they are first reckoned, as an
    /// entry is when it is made.
    pub held: Option<Totals>,
}

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
        let created_bytes = volume_bytes.saturating_add(stream_bytes);
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

/// Which side of an entry a declaration would exceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The reservations: guaranteed capacity.
    Reservation,
    /// The limits: sold capacity.
    Limit,
}

impl Side {
    /// The side as a refusal names it: `reservations` or `limits`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reservation => "reservations",
            Self::Limit => "limits",
        }
    }
}

/// Why a declaration is not admitted. It names the set's owner and its numbers, so it is for
/// whoever may read that owner: a caller who may not is told less (Finding 140 of
/// docs/23-walkthroughs.md).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the quota set of {owner} bounds {} {} at {bound}; this would hold {would}",
        dimension.as_str(), side.as_str())]
pub struct Exceeded {
    /// The entry.
    pub entry: Uuid,
    /// Whose set the entry is of.
    pub owner: QuotaOwner,
    /// The dimension.
    pub dimension: Dimension,
    /// Reservations or limits.
    pub side: Side,
    /// The entry's bound.
    pub bound: u64,
    /// What would be held, saturating; `u64::MAX` for an unbounded limit.
    pub would: u64,
}

impl Exceeded {
    /// The refusal without its numbers: the set and the dimension, for a reader who may not read
    /// the set's owner, to whom its bound and what everything beneath it holds are not told.
    #[must_use]
    pub fn unnumbered(&self) -> String {
        let (dimension, side) = (self.dimension.as_str(), self.side.as_str());
        format!(
            "the quota set of {} bounds {dimension} {side}, and this would exceed it",
            self.owner
        )
    }
}

impl QuotaEntry {
    /// This entry with the bounds and the egress budget of `other`, which has its shape; what
    /// this entry's budget was reckoned to have spent is kept.
    #[must_use]
    pub fn with_bounds(self, other: Self) -> Self {
        let spent = self.egress.and_then(|b| b.spent);
        let egress = other.egress.map(|b| EgressBudget { spent, ..b });
        Self { reservations: other.reservations, limits: other.limits, egress, ..self }
    }

    /// Whether `other`, of this entry's shape, says what this entry says: the same bounds, and
    /// the same budget with the same exhaustion.
    #[must_use]
    pub fn same_terms(&self, other: &Self) -> bool {
        let terms = |e: &Self| e.egress.map(|b| (b.bytes_per_month, b.exhausted));
        self.reservations == other.reservations
            && self.limits == other.limits
            && terms(self) == terms(other)
    }

    /// Whether the entry counts a workload with `labels`.
    #[must_use]
    pub fn counts(&self, labels: &BTreeMap<String, String>) -> bool {
        self.selector.as_ref().is_none_or(|s| s.matches(labels))
    }

    /// Whether the entry counts what is placed on a node with `labels`; an entry that names no
    /// nodes counts nothing by placement.
    #[must_use]
    pub fn counts_on(&self, labels: &BTreeMap<String, String>) -> bool {
        self.nodes.as_ref().is_some_and(|s| s.matches(labels))
    }

    /// Admits a workload that held `old` (nothing, if new) and would hold `new`, beside `rest`,
    /// what everything else the entry counts holds. Only growth is refused: a change that holds
    /// no more than before passes even where the entry is already exceeded, so lowering a bound
    /// never strands a workload that cannot then be edited.
    pub fn admit(&self, rest: &Totals, old: Option<&Held>, new: &Held) -> Result<(), Exceeded> {
        for (d, bound) in &self.reservations {
            let mine = new.reservations.get(d).copied().unwrap_or(0);
            let before = old.and_then(|o| o.reservations.get(d).copied()).unwrap_or(0);
            let would = rest.reserved(*d).saturating_add(mine);
            if mine > before && would > *bound {
                return Err(self.exceeded(*d, Side::Reservation, *bound, would));
            }
        }
        for (d, bound) in &self.limits {
            let mine = new.limits.get(d).copied().unwrap_or(Some(0));
            let before = old.map_or(Some(0), |o| o.limits.get(d).copied().unwrap_or(Some(0)));
            let rest = rest.limited(*d);
            let grew = match (mine, before) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(m), Some(b)) => m > b,
            };
            let would = match (rest, mine) {
                (Some(r), Some(m)) => r.saturating_add(m),
                _ => u64::MAX,
            };
            if grew && would > *bound {
                return Err(self.exceeded(*d, Side::Limit, *bound, would));
            }
        }
        Ok(())
    }

    fn exceeded(&self, dimension: Dimension, side: Side, bound: u64, would: u64) -> Exceeded {
        Exceeded { entry: self.id, owner: self.owner, dimension, side, bound, would }
    }
}
