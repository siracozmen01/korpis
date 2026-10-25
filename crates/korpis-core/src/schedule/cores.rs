// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which of a node's cores a workload asking for dedicated cores is given (§5.2 of
//! docs/05-scheduling.md): those its placement following its target in place holds first, then
//! cores of the NUMA node with the most free, then the lowest ids; never a core another placement
//! holds. A placement reserves what its cores take on its node, every logical CPU of each, rather
//! than the CPU it declared.

use super::{Request, reservation};
use crate::node::Node;
use crate::node::cores::{Core, MILLIS_PER_CPU};
use crate::tenancy::quota::{Bounds, Dimension};
use crate::workload::IntentBody;
use crate::workload::spec::resources::CpuIsolation;
use std::collections::BTreeMap;

/// How many cores `body` asks for: none unless its CPU is dedicated, else one for each 1,000
/// millicores it reserves.
#[must_use]
pub fn asked(body: &IntentBody) -> usize {
    let cpu = &body.resources.cpu;
    if cpu.isolation != CpuIsolation::DedicatedCores {
        return 0;
    }
    usize::try_from(cpu.reservation_millis / MILLIS_PER_CPU).unwrap_or(usize::MAX)
}

/// The `count` cores of `cores` given beside `held`, the cores the node's other placements hold:
/// those `keep` names first, then those of the NUMA node with the most free, then the lowest ids.
///
/// # Errors
/// Too few are free, for the explanation.
pub fn choose(
    cores: &[Core],
    held: &[u32],
    count: usize,
    keep: &[u32],
) -> Result<Vec<u32>, String> {
    let free: Vec<&Core> = cores.iter().filter(|c| !held.contains(&c.id)).collect();
    if free.len() < count {
        return Err(format!("{} of {count} cores free", free.len()));
    }
    let mut per_node: BTreeMap<u32, usize> = BTreeMap::new();
    for c in &free {
        *per_node.entry(c.numa).or_default() += 1;
    }
    let mut ordered = free;
    ordered.sort_by_key(|c| {
        let room = per_node.get(&c.numa).copied().unwrap_or(0);
        (!keep.contains(&c.id), std::cmp::Reverse(room), c.numa, c.id)
    });
    let mut given: Vec<u32> = ordered.into_iter().take(count).map(|c| c.id).collect();
    given.sort_unstable();
    Ok(given)
}

/// The cores `req` is given on `node`, beside what the node's other placements hold, keeping
/// those its current placement there holds: what the filter finds free, and the bind records.
///
/// # Errors
/// Too few are free.
pub fn given(req: &Request<'_>, node: &Node) -> Result<Vec<u32>, String> {
    let count = asked(req.body);
    if count == 0 {
        return Ok(Vec::new());
    }
    let held = req.cores.get(&node.id).map_or(&[][..], Vec::as_slice);
    let keep = req.current.filter(|c| c.node == node.id).map_or(&[][..], |c| c.cores);
    choose(&node.cores, held, count, keep)
}

/// What a placement of `body` reserves on `node` holding `cores`: its reservation, its CPU
/// replaced by every logical CPU of those cores where it holds any.
#[must_use]
pub fn reservation_on(body: &IntentBody, node: &Node, cores: &[u32]) -> Bounds {
    let mut reserved = reservation(body);
    if !cores.is_empty() {
        let millis =
            node.cores.iter().filter(|c| cores.contains(&c.id)).map(Core::millis).sum::<u64>();
        reserved.insert(Dimension::Cpu, millis);
    }
    reserved
}

#[cfg(test)]
mod tests;
