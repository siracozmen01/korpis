// SPDX-License-Identifier: AGPL-3.0-or-later

//! The dedicated cores a placement holds here (§5.2 of docs/05-scheduling.md): given to the
//! driver from the node's own topology, never from what the control plane says a core is, and
//! kept out of every other workload's cpuset by the helper, which is told the CPUs held before
//! an object holding cores is made and after one is destroyed.

use super::act::Failure;
use super::{Reconciler, Tracked};
use crate::held::HeldSet;
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};

/// The CPUs held, by the cgroup parent of the driver whose objects hold them.
type ByParent = BTreeMap<String, BTreeSet<u32>>;

/// The CPUs the helper was last told are held, and a set it refused, which is not asked again
/// until what is held changes.
#[derive(Debug, Default)]
pub(super) struct Kept {
    written: Option<ByParent>,
    refused: Option<ByParent>,
}

/// The cores `ids` names, as the node offers them; an id it does not offer is refused.
fn offered(cores: &[pb::Core], ids: &[u32]) -> Result<Vec<pb::Core>, String> {
    ids.iter()
        .map(|id| {
            let core = cores.iter().find(|c| c.id == *id);
            core.cloned().ok_or_else(|| format!("core {id} is not one this node offers"))
        })
        .collect()
}

/// The logical CPUs of `cores`.
fn cpus(cores: &[pb::Core]) -> Vec<u32> {
    cores.iter().flat_map(|c| c.cpus.iter().copied()).collect()
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Fills in the cores `a`'s placement holds here and tells the helper they are held before
    /// the object is made. A core the node no longer offers, or a driver that declares no cgroup
    /// parent to hold one under, is refused as a driver refuses a target, before anything is
    /// made, so a workload not yet made here is placed elsewhere.
    pub(super) async fn dedicate(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        spec: &mut driver::PreparedSpec,
    ) -> Result<(), Failure> {
        if a.cores.is_empty() {
            return Ok(());
        }
        let parent = self.wiring.driver_kinds.cgroup_parent(super::driver_of(a));
        let parent = parent.ok_or_else(|| format!("{} holds no cores", super::driver_of(a)));
        let given = parent.and_then(|p| Ok((p, offered(&self.wiring.cores, &a.cores)?)));
        let (parent, cores) = given.map_err(|why| {
            let reason = pb::UnsatisfiableReason::UNSATISFIABLE_REASON_INSUFFICIENT_RESOURCES;
            let detail = why.clone();
            t.rejected =
                Some(driver::Rejection { reason: reason.into(), detail, ..Default::default() });
            Failure(why)
        })?;
        t.cores = (parent.clone(), cpus(&cores));
        let mut held = self.cores_held();
        held.entry(parent).or_default().extend(t.cores.1.iter().copied());
        self.hold_cores(held).await.map_err(|e| Failure(format!("the cores held: {e}")))?;
        spec.cores = cores;
        Ok(())
    }

    /// The CPUs of every core an object here holds, under every parent the drivers declare.
    fn cores_held(&self) -> ByParent {
        let parents = self.wiring.driver_kinds.cgroup_parents();
        let mut held: ByParent = parents.into_iter().map(|p| (p, BTreeSet::new())).collect();
        let live =
            |t: &&Tracked| t.runtime.is_some() || t.runs.values().any(|r| !r.runtime.is_empty());
        for (parent, cpus) in self.tracked.values().filter(live).map(|t| &t.cores) {
            if let Some(h) = held.get_mut(parent) {
                h.extend(cpus.iter().copied());
            }
        }
        held
    }

    /// Keeps the helper's cpusets to the cores objects here hold, once a pass: an object adopted
    /// holds its assignment's, and one destroyed none. Nothing is written on a node that offers
    /// no cores or whose drivers declare no cgroup parent.
    pub(super) async fn keep_cores(&mut self, held: &HeldSet) {
        if self.wiring.cores.is_empty() || self.wiring.driver_kinds.cgroup_parents().is_empty() {
            return;
        }
        for (workload, t) in &mut self.tracked {
            let Some(a) = held.assignments.get(workload).filter(|_| t.cores.1.is_empty()) else {
                continue;
            };
            let parent = self.wiring.driver_kinds.cgroup_parent(super::driver_of(a));
            if let (Some(p), Ok(cores)) = (parent, offered(&self.wiring.cores, &a.cores)) {
                t.cores = (p, cpus(&cores));
            }
        }
        let cpus = self.cores_held();
        if self.cores_kept.refused.as_ref() == Some(&cpus) {
            return;
        }
        if let Err(e) = self.hold_cores(cpus.clone()).await {
            tracing::warn!(error = %e, "the helper did not keep the cores held");
            self.cores_kept.refused = Some(cpus);
        }
    }

    /// Tells the helper `cpus` are held, unless it was last told just that.
    async fn hold_cores(&mut self, cpus: ByParent) -> Result<(), crate::host::HostError> {
        if self.cores_kept.written.as_ref() == Some(&cpus) {
            return Ok(());
        }
        let parents = cpus.iter().map(|(parent, held)| helper::HeldCores {
            parent: parent.clone(),
            cpus: held.iter().copied().collect(),
            ..Default::default()
        });
        let request = helper::HoldCoresRequest { parents: parents.collect(), ..Default::default() };
        self.host.hold_cores(&request).await?;
        self.cores_kept = Kept { written: Some(cpus), refused: None };
        Ok(())
    }
}

#[cfg(test)]
mod tests;
