// SPDX-License-Identifier: Apache-2.0

//! Dedicated cores as the node gives them (§5.2 of docs/05-scheduling.md, §4.2 of
//! docs/04-runtimes.md): a core of the machine read from the kernel's topology, as an agent reads
//! one to offer it, and the cpusets the helper keeps under the driver's cgroup parent while an
//! object holds it. The parent's `shared` cgroup is kept off its CPUs, and they are written as the
//! exclusive CPUs of every cgroup from below the root down to the parent's `dedicated` one, from
//! the top down, which is where a driver's partitions take theirs from; writing them takes no CPU
//! from anyone until a partition is made. The suite reads the topology itself, depending on
//! nothing of Korpis's but the protocol.

use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// The kernel's CPU topology.
const SYSTEM: &str = "/sys/devices/system";
/// The cgroup v2 root.
const CGROUPS: &str = "/sys/fs/cgroup";
/// The most logical CPUs a list is believed to name.
const MOST_CPUS: u32 = 8192;

/// The text of `path`, trimmed, if it is there.
fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

/// The CPUs a kernel CPU list (`0-3,8`) names; `None` for one that is not.
#[must_use]
pub fn cpu_list(s: &str) -> Option<BTreeSet<u32>> {
    let mut cpus = BTreeSet::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        let (first, last) = part.split_once('-').unwrap_or((part, part));
        let (first, last) = (first.trim().parse::<u32>().ok()?, last.trim().parse::<u32>().ok()?);
        if first > last || last >= MOST_CPUS {
            return None;
        }
        cpus.extend(first..=last);
    }
    Some(cpus)
}

/// `cpus` as a list the kernel reads.
#[must_use]
pub fn listed(cpus: &BTreeSet<u32>) -> String {
    cpus.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

/// The machine's last core, the one holding its highest online CPU: its logical CPUs, every one
/// online, and the NUMA node of the first; none where that core holds CPU 0, as on a machine of
/// one core, or the topology cannot be read.
#[must_use]
pub fn last_core() -> Option<pb::Core> {
    let system = Path::new(SYSTEM);
    let online = cpu_list(&read(&system.join("cpu/online"))?)?;
    let last = *online.last()?;
    let topology = system.join(format!("cpu/cpu{last}/topology"));
    let siblings = read(&topology.join("core_cpus_list"))
        .or_else(|| read(&topology.join("thread_siblings_list")))
        .and_then(|l| cpu_list(&l))?;
    if !siblings.contains(&last) || siblings.contains(&0) || !siblings.is_subset(&online) {
        return None;
    }
    let id = *siblings.first()?;
    let numa = std::fs::read_dir(system.join("node")).ok().and_then(|nodes| {
        nodes.filter_map(Result::ok).find_map(|n| {
            let name = n.file_name().to_string_lossy().into_owned();
            let node = name.strip_prefix("node")?.parse::<u32>().ok()?;
            let cpus = cpu_list(&read(&n.path().join("cpulist"))?)?;
            cpus.contains(&id).then_some(node)
        })
    });
    let cpus = siblings.into_iter().collect();
    Some(pb::Core { id, cpus, numa: numa.unwrap_or(0), ..Default::default() })
}

/// `value` written to the cgroup file `path`.
fn write(path: &Path, value: &str) -> Result<(), String> {
    std::fs::write(path, value).map_err(|e| format!("{}: {e}", path.display()))
}

/// The cpusets kept for an object holding some CPUs under a driver's cgroup parent, as the helper
/// keeps them; each file put back as it was when this is dropped, the deepest first.
#[derive(Debug)]
pub struct Held {
    /// Each cgroup file written, and what it held before.
    written: Vec<(PathBuf, String)>,
}

impl Held {
    /// Keeps the cpusets under the cgroup `parent` for an object holding `cpus`.
    ///
    /// # Errors
    /// A cgroup could not be made or written, or `cpus` are every CPU online.
    pub fn hold(parent: &str, cpus: &BTreeSet<u32>) -> Result<Self, String> {
        let root = Path::new(CGROUPS);
        let online = read(&Path::new(SYSTEM).join("cpu/online")).and_then(|o| cpu_list(&o));
        let online = online.ok_or("the CPUs online could not be read")?;
        let shared: BTreeSet<u32> = online.difference(cpus).copied().collect();
        if shared.is_empty() {
            return Err("every CPU online would be held".into());
        }
        let mut chain = Vec::new();
        let mut at = root.to_owned();
        let below = Path::new(parent).components().filter(|c| matches!(c, Component::Normal(_)));
        for part in below.map(Component::as_os_str).chain([std::ffi::OsStr::new("dedicated")]) {
            at.push(part);
            chain.push(at.clone());
        }
        let shared_at = at.with_file_name("shared");
        for dir in chain.iter().chain([&shared_at]) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut held = Self { written: Vec::new() };
        for dir in &chain {
            write(&dir.join("cgroup.subtree_control"), "+cpuset")?;
        }
        held.put(&shared_at.join("cpuset.cpus"), &listed(&shared))?;
        for dir in &chain {
            held.put(&dir.join("cpuset.cpus.exclusive"), &listed(cpus))?;
        }
        Ok(held)
    }

    /// Writes `value` to `file`, keeping what it held to put back.
    fn put(&mut self, file: &Path, value: &str) -> Result<(), String> {
        let before = read(file).ok_or_else(|| format!("{} could not be read", file.display()))?;
        write(file, value)?;
        self.written.push((file.to_owned(), before));
        Ok(())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        for (file, before) in self.written.iter().rev() {
            let _ = write(file, before);
        }
    }
}

/// Whether the object holding `cpus` under the cgroup `parent` runs on them alone, as the kernel
/// says: a cgroup under the parent's `dedicated` one whose effective CPUs are exactly `cpus`, and
/// the parent's `shared` cgroup on none of them.
///
/// # Errors
/// Neither holds, saying what was found.
pub fn holding(parent: &str, cpus: &BTreeSet<u32>) -> Result<(), String> {
    let at = Path::new(CGROUPS).join(parent.trim_start_matches('/'));
    let effective =
        |dir: &Path| read(&dir.join("cpuset.cpus.effective")).and_then(|l| cpu_list(&l));
    let shared =
        effective(&at.join("shared")).ok_or("the shared cgroup's CPUs could not be read")?;
    if !shared.is_disjoint(cpus) {
        return Err(format!("the shared cgroup runs on CPUs {} of those held", listed(&shared)));
    }
    let objects = std::fs::read_dir(at.join("dedicated")).map_err(|e| format!("dedicated: {e}"))?;
    let found: Vec<BTreeSet<u32>> =
        objects.filter_map(Result::ok).filter_map(|o| effective(&o.path())).collect();
    if found.iter().any(|f| f == cpus) {
        return Ok(());
    }
    let found: Vec<String> = found.iter().map(listed).collect();
    Err(format!("no cgroup under dedicated on CPUs {} alone: {found:?}", listed(cpus)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_cpu_list_is_read_as_the_kernel_writes_it_and_written_as_it_reads_it() {
        assert_eq!(cpu_list("2-3,6,8-9\n").unwrap(), [2, 3, 6, 8, 9].into_iter().collect());
        assert_eq!(cpu_list("").unwrap(), BTreeSet::new());
        assert!(cpu_list("3-2").is_none() && cpu_list("x").is_none() && cpu_list("9000").is_none());
        assert_eq!(listed(&[7, 3].into_iter().collect()), "3,7");
    }
}
