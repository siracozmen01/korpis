// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workloads' cpusets for dedicated cores (§5.2 of docs/05-scheduling.md). Under each driver's
//! cgroup parent a request names, and only one the helper was configured to keep, it keeps a
//! `shared` cgroup on the node's online CPUs less every one held, so no workload holding no cores
//! runs on one held, and writes the CPUs held there as the exclusive CPUs of that cgroup, those
//! above it, and its `dedicated` cgroup, which a driver's cpuset partitions take theirs from; the
//! kernel uses those only once a partition is made, so writing them takes no CPU from anyone (§5
//! of docs/research/evidence.md).

use crate::error::{HelperError, HelperResult};
use korpis_topology::listed;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Where the cgroups are, and which the helper keeps.
#[derive(Debug, Clone)]
pub struct CoresConfig {
    /// The cgroup v2 root, `/sys/fs/cgroup` on a node.
    pub root: PathBuf,
    /// The CPUs online, `/sys/devices/system/cpu/online` on a node.
    pub online: PathBuf,
    /// The drivers' cgroup parents a request may name, below `root`: `/korpis` for the `oci`
    /// driver.
    pub parents: Vec<PathBuf>,
}

/// The CPUs a kernel list names.
fn parse(s: &str) -> HelperResult<BTreeSet<u32>> {
    korpis_topology::cpu_list(s).map_err(HelperError::Failed)
}

fn failed(path: &Path, e: &std::io::Error) -> HelperError {
    HelperError::Failed(format!("{}: {e}", path.display()))
}

fn write(path: &Path, value: &str) -> HelperResult<()> {
    std::fs::write(path, value).map_err(|e| failed(path, &e))
}

/// The cores' cgroups.
#[derive(Debug, Clone)]
pub struct Cores {
    config: CoresConfig,
}

impl Cores {
    /// The helper's cgroups, as `config` names them.
    ///
    /// # Errors
    /// A parent that is not a clean absolute path below the root.
    pub fn new(config: CoresConfig) -> HelperResult<Self> {
        let clean = |p: &PathBuf| {
            p.is_absolute()
                && p.components().count() > 1
                && p.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        };
        if let Some(p) = config.parents.iter().find(|p| !clean(p)) {
            return Err(HelperError::Refused(format!(
                "{} is no cgroup below the root",
                p.display()
            )));
        }
        Ok(Self { config })
    }

    /// Keeps the cpusets under each parent `held` names to the CPUs held there, the logical CPUs
    /// of every core its objects hold; what the `shared` cgroups were left on, the online CPUs
    /// less every one held.
    ///
    /// # Errors
    /// A parent the helper was not configured to keep, a CPU not online, held under two parents,
    /// or every one held, or a cgroup file that could not be written.
    pub fn hold(&self, held: &BTreeMap<String, BTreeSet<u32>>) -> HelperResult<Vec<u32>> {
        let kept = |p: &&String| self.config.parents.iter().any(|k| k == Path::new(p));
        if let Some(p) = held.keys().find(|p| !kept(p)) {
            return Err(HelperError::Refused(format!("{p} is no cgroup kept")));
        }
        let text = std::fs::read_to_string(&self.config.online)
            .map_err(|e| HelperError::Failed(format!("the CPUs online: {e}")))?;
        let online = parse(&text)?;
        let mut all = BTreeSet::new();
        for cpus in held.values() {
            if !cpus.is_subset(&online) || !cpus.is_disjoint(&all) {
                return Err(HelperError::Refused("a CPU held is not online, or held twice".into()));
            }
            all.extend(cpus.iter().copied());
        }
        let shared: BTreeSet<u32> = online.difference(&all).copied().collect();
        if shared.is_empty() {
            return Err(HelperError::Refused("every CPU held leaves none shared".into()));
        }
        let want = self.exclusive(held)?;
        self.write_all(&want, &shared, held.keys())?;
        Ok(shared.into_iter().collect())
    }

    /// The exclusive CPUs each cgroup is to have: every one from below the root down to each
    /// parent, and the parent's `dedicated`, holding those of the parents beneath it. Each is
    /// made, with the cpuset controller on for its children, from the top down.
    fn exclusive(
        &self,
        held: &BTreeMap<String, BTreeSet<u32>>,
    ) -> HelperResult<BTreeMap<PathBuf, BTreeSet<u32>>> {
        let mut want: BTreeMap<PathBuf, BTreeSet<u32>> = BTreeMap::new();
        for (parent, cpus) in held {
            let mut at = self.config.root.clone();
            let below = Path::new(parent).components().skip(1).map(Component::as_os_str);
            for part in below.chain([std::ffi::OsStr::new("dedicated")]) {
                at.push(part);
                want.entry(at.clone()).or_default().extend(cpus.iter().copied());
            }
            let shared = at.with_file_name("shared");
            std::fs::create_dir_all(&shared).map_err(|e| failed(&shared, &e))?;
        }
        for at in want.keys() {
            std::fs::create_dir_all(at).map_err(|e| failed(at, &e))?;
            write(&at.join("cgroup.subtree_control"), "+cpuset")?;
        }
        Ok(want)
    }

    /// Writes `want` and the shared cpusets in the order the kernel requires: every exclusive
    /// set shrunk to what stays, from the deepest cgroup up, so no sibling's growth meets a CPU
    /// another still has; the shared cgroups narrowed or widened; then every exclusive set grown,
    /// from the top down.
    fn write_all<'a>(
        &self,
        want: &BTreeMap<PathBuf, BTreeSet<u32>>,
        shared: &BTreeSet<u32>,
        parents: impl Iterator<Item = &'a String>,
    ) -> HelperResult<()> {
        let mut order: Vec<(&PathBuf, &BTreeSet<u32>)> = want.iter().collect();
        order.sort_by_key(|(at, _)| std::cmp::Reverse(at.components().count()));
        for (at, cpus) in &order {
            let file = at.join("cpuset.cpus.exclusive");
            let before = std::fs::read_to_string(&file)
                .map_or_else(|_| Ok(BTreeSet::new()), |t| parse(&t))?;
            write(&file, &listed(before.intersection(cpus)))?;
        }
        for parent in parents {
            let at = self.config.root.join(parent.trim_start_matches('/'));
            write(&at.join("shared/cpuset.cpus"), &listed(shared))?;
        }
        for (at, cpus) in order.iter().rev() {
            write(&at.join("cpuset.cpus.exclusive"), &listed(*cpus))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
