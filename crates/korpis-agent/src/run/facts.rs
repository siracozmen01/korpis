// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node reports of itself (§2.1 of docs/05-scheduling.md): this machine's capacity and what
//! placement may reserve of it, its drivers and storage as each declares itself, the cores it
//! offers, its uplinks' bandwidth, and its devices.

use crate::advertise::Advertise;
use crate::host::Host;
use crate::runtime::{Runtime, RuntimeError};
use crate::session::Facts;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;

/// What the node keeps back from placement for its own system, agent, and runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reserve {
    /// Millicores.
    pub cpu_millis: u64,
    /// Bytes.
    pub memory_bytes: u64,
}

impl Default for Reserve {
    fn default() -> Self {
        Self { cpu_millis: 500, memory_bytes: 1 << 30 }
    }
}

/// Total memory, from `/proc/meminfo`; zero where it cannot be read.
fn memory_total() -> u64 {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    text.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kib| kib.saturating_mul(1024))
}

/// This machine's capacity, and what placement may reserve of it: capacity less `reserve`,
/// each part of which is capped at a quarter of capacity so a small machine is not reserved away
/// (§2.1 of docs/05-scheduling.md).
#[must_use]
pub fn machine(reserve: Reserve) -> (pb::Resources, pb::Resources) {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let cpu = u64::try_from(cores).unwrap_or(1).saturating_mul(1000);
    let memory = memory_total();
    let capacity = pb::Resources { cpu_millis: cpu, memory_bytes: memory, ..Default::default() };
    let allocatable = pb::Resources {
        cpu_millis: cpu.saturating_sub(reserve.cpu_millis.min(cpu / 4)),
        memory_bytes: memory.saturating_sub(reserve.memory_bytes.min(memory / 4)),
        ..Default::default()
    };
    (capacity, allocatable)
}

/// The facts a node reports: its drivers, each as it declares itself, its storage, as the
/// helper declares it, this machine and the `cores` it offers, the bandwidth of its `uplinks`
/// once the helper shapes them (§7 of docs/07-networking.md), and its `devices` as they are
/// when asked. A helper that cannot be reached leaves the node offering no storage class and no
/// bandwidth rather than keeping it from starting.
pub async fn facts_of<R: Runtime, H: Host>(
    runtime: &R,
    host: &H,
    (reserve, uplinks, cores): (Reserve, &[helper::Uplink], Vec<pb::Core>),
    (advertise, devices): (Advertise, crate::devices::Devices),
) -> Result<Arc<dyn Fn() -> Facts + Send + Sync>, RuntimeError> {
    let mut drivers = runtime.drivers().await?;
    let storage_classes = host.storage().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the helper did not declare its storage; offering none");
        Vec::new()
    });
    let (mut capacity, mut allocatable) = machine(reserve);
    // Disk is what the storage classes hold, and what is free in them now: anything already
    // on their filesystems, this node's own volumes included, counts as taken (§2.1 of
    // docs/05-scheduling.md).
    capacity.disk_bytes = storage_classes.iter().map(|c| c.capacity_bytes).sum();
    allocatable.disk_bytes = storage_classes.iter().map(|c| c.free_bytes).sum();
    let held = crate::shaping::held(host, uplinks).await;
    for driver in &mut drivers {
        crate::shaping::enforced(driver, &held.kinds);
    }
    if let Some((offered, link)) = held.bandwidth {
        (allocatable.bandwidth_bits_per_sec, capacity.bandwidth_bits_per_sec) = (offered, link);
    }
    let advertised = agent::Advertised::default();
    let facts = Facts {
        drivers,
        storage_classes,
        capacity,
        allocatable,
        advertised,
        cores,
        ..Facts::default()
    };
    Ok(Arc::new(move || {
        let held = Arc::clone(&devices.borrow());
        Facts {
            advertised: advertise.current(),
            devices: held.devices.clone(),
            unoffered_devices: held.unoffered.clone(),
            ..facts.clone()
        }
    }))
}

/// The facts the node reports when it starts, and what the reconciler takes from them: what
/// each driver is, and the cores it offers for dedication, neither of which changes while it runs.
pub(super) fn declared(
    facts: &(dyn Fn() -> Facts + Send + Sync),
) -> (Facts, crate::reconcile::DriverKinds, Vec<pb::Core>) {
    let now = facts();
    let (kinds, cores) = (crate::reconcile::DriverKinds::of(&now.drivers), now.cores.clone());
    (now, kinds, cores)
}
