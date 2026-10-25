// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's physical cores (§5.2 of docs/05-scheduling.md), read from the kernel's topology under
//! `/sys/devices/system`: each core the logical CPUs that share its execution units, whole, and
//! only when all of them are online; never the core holding CPU 0. The agent reads them to offer
//! them, and a driver reads them again to check the cores it is given, so neither believes what
//! the control plane says a core is (§4.2 of docs/04-runtimes.md).

use korpis_core::node::cores::Core;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The most logical CPUs read; a CPU list naming more is not believed.
const MOST_CPUS: u32 = 8192;
/// The most cores a node reports, and logical CPUs one core holds, as its facts carry them.
const MOST_CORES: usize = 1024;
const MOST_SIBLINGS: usize = 8;

/// The CPUs a kernel CPU list (`0-3,8,10-11`) names.
///
/// # Errors
/// It is not one, or names a CPU past the most read.
pub fn cpu_list(s: &str) -> Result<BTreeSet<u32>, String> {
    let mut cpus = BTreeSet::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        let (first, last) = part.split_once('-').unwrap_or((part, part));
        let parse = |n: &str| n.trim().parse::<u32>().map_err(|_| format!("{s:?} is no CPU list"));
        let (first, last) = (parse(first)?, parse(last)?);
        if first > last || last >= MOST_CPUS {
            return Err(format!("{s:?} is no CPU list of at most {MOST_CPUS} CPUs"));
        }
        cpus.extend(first..=last);
    }
    Ok(cpus)
}

/// `cpus` as the kernel writes a CPU list: `1-3,7`, and empty for none.
#[must_use]
pub fn listed<'a>(cpus: impl IntoIterator<Item = &'a u32>) -> String {
    let set: BTreeSet<u32> = cpus.into_iter().copied().collect();
    let range = |(first, last): (u32, u32)| {
        if first == last { first.to_string() } else { format!("{first}-{last}") }
    };
    let mut out: Vec<String> = Vec::new();
    let mut run: Option<(u32, u32)> = None;
    for c in set {
        run = match run {
            Some((first, last)) if last + 1 == c => Some((first, c)),
            Some(r) => {
                out.push(range(r));
                Some((c, c))
            }
            None => Some((c, c)),
        };
    }
    out.extend(run.map(range));
    out.join(",")
}

/// The text of `path`, if it is there.
fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The NUMA node of each CPU, from `system/node/nodeN/cpulist`; a CPU of none is node 0's.
fn numa_nodes(system: &Path) -> BTreeMap<u32, u32> {
    let mut of = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(system.join("node")) else { return of };
    for e in entries.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(node) = name.strip_prefix("node").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let cpus = read(&e.path().join("cpulist")).and_then(|l| cpu_list(&l).ok());
        of.extend(cpus.unwrap_or_default().into_iter().map(|c| (c, node)));
    }
    of
}

/// The cores under `system` (`/sys/devices/system` on a node) that may be dedicated: those
/// `named` names, when it names any, never the one holding CPU 0, and each only with every one of
/// its logical CPUs online. A topology that cannot be read has none.
#[must_use]
pub fn cores(system: &Path, named: Option<&BTreeSet<u32>>) -> Vec<Core> {
    let online = read(&system.join("cpu/online")).and_then(|l| cpu_list(&l).ok());
    let Some(online) = online else { return Vec::new() };
    let numa = numa_nodes(system);
    let mut found: BTreeSet<Vec<u32>> = BTreeSet::new();
    for cpu in &online {
        let topology = system.join(format!("cpu/cpu{cpu}/topology"));
        let siblings = read(&topology.join("core_cpus_list"))
            .or_else(|| read(&topology.join("thread_siblings_list")))
            .and_then(|l| cpu_list(&l).ok());
        if let Some(s) = siblings.filter(|s| s.contains(cpu)) {
            found.insert(s.into_iter().collect());
        }
    }
    // Within what a node's facts carry (korpis-wire's bounds), and no CPU in two cores, which a
    // topology read mid-change could otherwise say: one bad core would refuse the whole report.
    let mut taken = BTreeSet::new();
    found
        .into_iter()
        .filter(|cpus| cpus.iter().all(|c| online.contains(c)) && !cpus.contains(&0))
        .filter(|cpus| {
            let apart = cpus.len() <= MOST_SIBLINGS && cpus.iter().all(|c| !taken.contains(c));
            taken.extend(cpus.iter().copied().filter(|_| apart));
            apart
        })
        .take(MOST_CORES)
        .filter_map(|cpus| {
            let id = *cpus.first()?;
            let numa = numa.get(&id).copied().unwrap_or(0);
            Some(Core { id, cpus, numa })
        })
        .filter(|c| named.is_none_or(|n| n.contains(&c.id)))
        .collect()
}

/// Whether each of `given` is a core of the machine under `system` as its topology reads now:
/// the same logical CPUs, every one online, on the same NUMA node, and not the core of CPU 0.
///
/// # Errors
/// The first that is not, and why.
pub fn check(system: &Path, given: &[Core]) -> Result<(), String> {
    let machine = cores(system, None);
    for g in given {
        let Some(found) = machine.iter().find(|c| c.id == g.id) else {
            return Err(format!("core {} is not one of this machine's that may be held", g.id));
        };
        if found.cpus != g.cpus || found.numa != g.numa {
            return Err(format!("core {} is not CPUs {:?} of NUMA node {}", g.id, g.cpus, g.numa));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
