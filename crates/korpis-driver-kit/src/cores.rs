// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated cores as a driver holds them (§5.2 of docs/05-scheduling.md, §4.2 of
//! docs/04-runtimes.md). An object holding cores runs in a cgroup of its own under
//! `<parent>/dedicated`, its `cpuset` its cores' logical CPUs and their NUMA nodes' memory, and
//! every other object under `<parent>/shared`, whose `cpuset` the node's helper keeps off the held
//! CPUs. The cores are checked against the machine's topology as the driver reads it, never as the
//! agent says it is, and by default the holding cgroup is made a remote cpuset partition before
//! anything of the object runs, which keeps the host's own processes off them too;
//! `--dedicated-cores-host shared` leaves the host on them, and the declaration says so.

use korpis_core::node::cores::Core;
use korpis_proto::korpis::v1 as pb;
use std::path::{Component, Path, PathBuf};

/// The unified cgroup hierarchy.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Whether a holding cgroup is made a partition, keeping the host off its cores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// The default: a remote cpuset partition of its own.
    Kept,
    /// `--dedicated-cores-host shared`: other tenants kept off, the host not.
    Shared,
}

/// Where the cores' cgroups are, and how they are held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cores {
    /// The kernel's topology, `/sys/devices/system` on a node.
    pub system: PathBuf,
    /// The cgroup v2 root, [`CGROUP_ROOT`] on a node.
    pub root: PathBuf,
    /// The cgroup the driver's objects live under, from the root: `/korpis` for the `oci` driver.
    pub parent: String,
    /// Whether the host is kept off.
    pub host: Host,
}

/// The `cpuset` an object's cores give it, as the kernel writes one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cpuset {
    /// Their logical CPUs: `2-3,10-11`.
    pub cpus: String,
    /// Their NUMA nodes' memory: `0`.
    pub mems: String,
}

/// `parent` as a cgroup parent: a clean absolute path below the root, `/korpis`.
///
/// # Errors
/// It is not one.
pub fn parent(parent: &str) -> Result<String, String> {
    let path = Path::new(parent);
    let clean = path.is_absolute()
        && path.components().count() > 1
        && path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if clean {
        Ok(parent.trim_end_matches('/').to_owned())
    } else {
        Err(format!("{parent:?} is no cgroup below the root"))
    }
}

impl Cores {
    /// The cgroup, from the root, an object `id` is made in: under `dedicated` when it holds
    /// cores, `shared` otherwise.
    #[must_use]
    pub fn cgroup(&self, id: &str, holds: bool) -> String {
        let under = if holds { "dedicated" } else { "shared" };
        format!("{}/{under}/{id}", self.parent)
    }

    /// The directory of the cgroup object `id` runs in: whichever of its places is there,
    /// `shared` when none is, as for an object not yet started. An object made before cores
    /// were held is directly under the parent.
    #[must_use]
    pub fn dir(&self, id: &str) -> PathBuf {
        let at = self.root.join(self.parent.trim_start_matches('/'));
        let places = [at.join("shared").join(id), at.join("dedicated").join(id), at.join(id)];
        let found = places.iter().find(|p| p.is_dir()).cloned();
        found.unwrap_or_else(|| at.join("shared").join(id))
    }

    /// The `cpuset` of `given`, the cores an object's placement holds, once each is checked
    /// against the machine's topology as read now; none when it holds none.
    ///
    /// # Errors
    /// A core the machine does not have, CPUs that are not that core's, or the core of CPU 0.
    pub fn given(&self, given: &[pb::Core]) -> Result<Option<Cpuset>, String> {
        if given.is_empty() {
            return Ok(None);
        }
        let cores: Vec<Core> =
            given.iter().map(|c| Core { id: c.id, cpus: c.cpus.clone(), numa: c.numa }).collect();
        korpis_topology::check(&self.system, &cores)?;
        let cpus = korpis_topology::listed(cores.iter().flat_map(|c| &c.cpus));
        let mems = korpis_topology::listed(cores.iter().map(|c| &c.numa));
        Ok(Some(Cpuset { cpus, mems }))
    }

    /// Makes the cgroup at `dir`, a holding object's, a remote cpuset partition of its CPUs
    /// before anything of it runs, unless the host shares them.
    ///
    /// # Errors
    /// The kernel did not make it, with its reason: the helper had not made the CPUs exclusive
    /// above it, or the kernel has no remote partitions.
    pub fn partition(&self, dir: &Path) -> Result<(), String> {
        if self.host == Host::Shared || !in_dedicated(dir) {
            return Ok(());
        }
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).map_err(|e| format!("{f}: {e}"));
        let write =
            |f: &str, v: &str| std::fs::write(dir.join(f), v).map_err(|e| format!("{f}: {e}"));
        let cpus = read("cpuset.cpus")?;
        write("cpuset.cpus.exclusive", cpus.trim())?;
        write("cpuset.cpus.partition", "root")?;
        match read("cpuset.cpus.partition")?.trim() {
            "root" => Ok(()),
            other => Err(format!("the kernel made no partition of {}: {other}", cpus.trim())),
        }
    }

    /// What the driver declares of dedicated cores: held, by which mechanism, with what it does
    /// not keep off; or not held, and why.
    #[must_use]
    pub fn declared(&self) -> pb::EnforcementDecl {
        let at = self.root.join(self.parent.trim_start_matches('/'));
        let outside = std::fs::read_dir(&at).ok().and_then(|entries| {
            entries.filter_map(Result::ok).find(|e| {
                let name = e.file_name();
                e.path().is_dir() && name != "shared" && name != "dedicated"
            })
        });
        let why_not = if let Some(e) = outside {
            Some(format!(
                "{} runs outside {}/shared, made before cores were held, and could run on one",
                e.file_name().to_string_lossy(),
                self.parent
            ))
        } else if self.host == Host::Kept && !partitions(&self.root) {
            Some("the kernel makes no remote cpuset partition here".to_owned())
        } else {
            None
        };
        let (mechanism, granularity) = match (&why_not, self.host) {
            (Some(why), _) => ("none", why.clone()),
            (None, Host::Kept) => (
                "cgroup v2 cpuset, each holder a remote cpuset partition",
                "whole cores, every thread; no other tenant and no host process runs on them, \
                 though per-CPU kernel threads and unsteered interrupts do"
                    .to_owned(),
            ),
            (None, Host::Shared) => (
                "cgroup v2 cpuset",
                "whole cores, every thread; no other tenant runs on them, and the host's own \
                 processes may (--dedicated-cores-host shared)"
                    .to_owned(),
            ),
        };
        pb::EnforcementDecl {
            enforced: why_not.is_none(),
            mechanism: mechanism.into(),
            granularity,
            ..Default::default()
        }
    }
}

/// Whether `dir` is a holding object's cgroup, under a `dedicated` cgroup.
fn in_dedicated(dir: &Path) -> bool {
    dir.parent().and_then(Path::file_name).is_some_and(|n| n == "dedicated")
}

/// Whether the kernel under `root` makes remote cpuset partitions: the cpuset controller given
/// to the root's children, and a child of it with the files a partition is made with.
fn partitions(root: &Path) -> bool {
    let control = std::fs::read_to_string(root.join("cgroup.subtree_control")).unwrap_or_default();
    if !control.split_whitespace().any(|c| c == "cpuset") {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return false };
    entries.filter_map(Result::ok).any(|e| {
        let p = e.path();
        p.join("cpuset.cpus.exclusive").is_file() && p.join("cpuset.cpus.partition").is_file()
    })
}

#[cfg(test)]
mod tests;
