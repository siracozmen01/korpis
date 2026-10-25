// SPDX-License-Identifier: AGPL-3.0-or-later

//! Adoption (§4.4 of docs/02-architecture.md): when the agent starts, every runtime object its
//! driver created is matched to the assignment its labels name and taken over as it is, touching
//! none of them; an object no assignment claims is an orphan, reported once and never deleted.

use super::{
    RECOVER_FIRST_WAIT, RECOVER_LONGEST_WAIT, Reconciler, Tracked, act, effect, runs, text,
};
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::driver::v1 as driver;
use std::time::Instant;

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Every runtime object the driver created, enumerated as often as it takes: nothing is
    /// reconciled before adoption, because acting without knowing what exists would start a
    /// second copy of what already runs (§4.4 of docs/02-architecture.md). `None` once the
    /// session has ended.
    pub(super) async fn recovered(&mut self) -> Option<Vec<driver::RuntimeObject>> {
        let mut wait = RECOVER_FIRST_WAIT;
        loop {
            match self.runtime.recover().await {
                Ok(objects) => return Some(objects),
                Err(e) => tracing::error!(
                    error = %e,
                    retry_in = ?wait,
                    "the driver could not enumerate its objects; nothing is reconciled until it can"
                ),
            }
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                e = self.wiring.events.recv() => if !self.heard(&e) { return None },
            }
            wait = (wait * 2).min(RECOVER_LONGEST_WAIT);
        }
    }

    /// Takes over every runtime object the driver created, touching none (§4.4 of
    /// docs/02-architecture.md); what no assignment claims is an orphan.
    pub(super) async fn adopt(&mut self, objects: Vec<driver::RuntimeObject>) {
        let held = self.wiring.held.borrow().clone();
        for o in objects {
            let workload = o.labels.as_option().map(|l| l.workload.clone()).unwrap_or_default();
            let claimed = held.assignments.contains_key(&workload);
            let run = o.labels.as_option().map(|l| l.run.clone()).unwrap_or_default();
            if claimed && !run.is_empty() {
                self.adopt_run(&workload, &run, &o);
                continue;
            }
            if !claimed || self.tracked.contains_key(&workload) {
                self.orphan(&o.runtime_id, &workload).await;
                continue;
            }
            let running = o.status.as_option().is_some_and(act::running);
            // The cores it holds are what its driver recorded making it with, not what its
            // assignment says now (Finding 172 of docs/23-walkthroughs.md).
            let a = held.assignments.get(&workload);
            let parent =
                a.and_then(|a| self.wiring.driver_kinds.cgroup_parent(super::driver_of(a)));
            let t = Tracked {
                runtime: Some(o.runtime_id.clone()),
                labels: o.labels.as_option().cloned(),
                status: o.status.as_option().cloned(),
                stopped_by_us: !running,
                running_since: running.then(Instant::now),
                cores: (parent.unwrap_or_default(), o.cpus.clone()),
                ..Tracked::default()
            };
            self.tracked.insert(workload.clone(), t);
            // A restarted agent serves the socket again at the path the container mounts.
            if let Some(Err(e)) = self.wiring.identity.as_ref().map(|i| i.ensure(&workload)) {
                tracing::error!(%workload, error = %e, "an identity socket could not be served");
            }
            let mut e = effect(Some(&workload), "node.adopt", true);
            e.after.insert("runtime_id".into(), text(&o.runtime_id));
            e.after.insert("running".into(), text(if running { "true" } else { "false" }));
            self.record(e).await;
        }
    }

    /// Takes over the object of `run` of `workload`, as it is: one running is tended from now,
    /// its timeout counted from when it started, and one that ended is reported once more,
    /// which the control plane takes once.
    fn adopt_run(&mut self, workload: &str, run: &str, o: &driver::RuntimeObject) {
        let parent = self
            .wiring
            .held
            .borrow()
            .assignments
            .get(workload)
            .and_then(|a| self.wiring.driver_kinds.cgroup_parent(super::driver_of(a)));
        let t = self.tracked.entry(workload.to_owned()).or_default();
        if t.cores.1.is_empty() && !o.cpus.is_empty() {
            t.cores = (parent.unwrap_or_default(), o.cpus.clone());
        }
        let held = runs::RunTracked { runtime: o.runtime_id.clone(), ..Default::default() };
        t.runs.insert(run.to_owned(), held);
    }

    /// Reports a runtime object no assignment claims, once: it is never deleted.
    pub(super) async fn orphan(&mut self, runtime_id: &str, workload: &str) {
        if !self.orphans.insert(runtime_id.to_owned()) {
            return;
        }
        tracing::warn!(%runtime_id, %workload, "orphaned runtime object; left untouched");
        let mut e = effect(None, "node.orphan", true);
        e.after.insert("runtime_id".into(), text(runtime_id));
        e.after.insert("workload_label".into(), text(workload));
        self.record(e).await;
    }
}
