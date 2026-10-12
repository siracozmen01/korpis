// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resources (§5.2 of docs/05-scheduling.md): what a workload reserves and what it is limited
//! to. A reservation is what placement guarantees; a limit is what the kernel enforces.

use super::SpecError;
use crate::recipe::derive::{Part, Resource};
use crate::value::int_to_f64;

/// A reservation and a limit; zero means unset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReservedLimit {
    /// Guaranteed by placement.
    pub reservation: u64,
    /// Enforced by the kernel.
    pub limit: u64,
}

impl ReservedLimit {
    /// The limit if set, otherwise the reservation.
    #[must_use]
    pub fn effective(self) -> u64 {
        if self.limit > 0 { self.limit } else { self.reservation }
    }

    fn check(self, field: &str) -> Result<(), SpecError> {
        if self.limit > 0 && self.reservation > self.limit {
            return Err(SpecError::new(field, "the reservation exceeds the limit"));
        }
        Ok(())
    }
}

/// How CPU is shared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CpuIsolation {
    /// Time-shared with other workloads.
    #[default]
    Shared,
    /// Whole cores, excluded from every other workload's cpuset.
    DedicatedCores,
}

/// CPU, in millicores.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuSpec {
    /// Guaranteed millicores.
    pub reservation_millis: u64,
    /// Enforced millicores.
    pub limit_millis: u64,
    /// How cores are shared.
    pub isolation: CpuIsolation,
}

/// Block I/O limits; zero means unset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IoSpec {
    /// Bytes read per second.
    pub read_bytes_per_sec: u64,
    /// Bytes written per second.
    pub write_bytes_per_sec: u64,
    /// Read operations per second.
    pub read_ops_per_sec: u64,
    /// Write operations per second.
    pub write_ops_per_sec: u64,
    /// Relative weight, 1 to 10000; zero means the default.
    pub share: u32,
}

/// Network bandwidth, in bits per second.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BandwidthSpec {
    /// Egress: a reservation and a limit.
    pub egress: ReservedLimit,
    /// Ingress: a limit only, because no node can guarantee inbound bandwidth.
    pub ingress_limit: u64,
}

/// Everything a workload reserves and is limited to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceSpec {
    /// CPU.
    pub cpu: CpuSpec,
    /// Memory, in bytes.
    pub memory: ReservedLimit,
    /// Disk, in bytes.
    pub disk: ReservedLimit,
    /// Block I/O.
    pub io: IoSpec,
    /// Network.
    pub bandwidth: BandwidthSpec,
    /// The process limit; zero means unset.
    pub pids_limit: u64,
}

impl ResourceSpec {
    /// Checks the internal consistency of the declaration.
    pub fn validate(&self) -> Result<(), SpecError> {
        let cpu = ReservedLimit {
            reservation: self.cpu.reservation_millis,
            limit: self.cpu.limit_millis,
        };
        cpu.check("resources.cpu")?;
        self.memory.check("resources.memory")?;
        self.disk.check("resources.disk")?;
        self.bandwidth.egress.check("resources.bandwidth.egress")?;
        if self.io.share > 10_000 {
            return Err(SpecError::new("resources.io.share", "1 to 10000"));
        }
        if self.cpu.isolation == CpuIsolation::DedicatedCores
            && (self.cpu.reservation_millis == 0
                || !self.cpu.reservation_millis.is_multiple_of(1000))
        {
            return Err(SpecError::new("resources.cpu", "dedicated cores reserve whole cores"));
        }
        Ok(())
    }
}

fn amount(n: u64) -> f64 {
    int_to_f64(i64::try_from(n).unwrap_or(i64::MAX))
}

impl ResourceSpec {
    /// One figure of the declaration, as a recipe's derived fields read it (§5.3 of
    /// docs/09-recipes.md): `Effective` is the limit if one is set, otherwise the reservation.
    #[must_use]
    pub fn read(&self, resource: Resource, part: Part) -> f64 {
        let rl = match resource {
            Resource::Cpu => ReservedLimit {
                reservation: self.cpu.reservation_millis,
                limit: self.cpu.limit_millis,
            },
            Resource::Memory => self.memory,
            Resource::Disk => self.disk,
            Resource::Pids => ReservedLimit { reservation: 0, limit: self.pids_limit },
        };
        amount(match part {
            Part::Effective => rl.effective(),
            Part::Reservation => rl.reservation,
            Part::Limit => rl.limit,
        })
    }
}
