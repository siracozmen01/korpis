// SPDX-License-Identifier: AGPL-3.0-or-later

//! One workload, one pass. What the reconciler may do is decided by the lease first (§4.5 of
//! docs/02-architecture.md): under a lease it executes the plan's steps and then keeps the
//! approved state; with the lease expired it changes nothing except bringing back a workload
//! that crashed, if it keeps running on expiry (§4.6), and stops it if it stops on expiry; when
//! another node holds the workload it stops it and keeps its data; and before a first lease it
//! waits. A retired workload's lease is released once its runtime object is gone.

use super::act::running;
use super::runs::has_runs;
use super::{Reconciler, Tracked, effect, text};
use crate::host::Host;
use crate::lease::{OnExpiry, Refusal, Standing};
use crate::runtime::{Runtime, RuntimeError};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, Instant};

/// The first wait before restarting a crashed workload; it doubles up to [`MOST`].
const FIRST: Duration = Duration::from_secs(1);
/// The longest wait between crash restarts.
const MOST: Duration = Duration::from_secs(300);
/// How long a workload must stay up for its past crashes to be forgiven.
const FORGIVEN: Duration = Duration::from_secs(60);

/// Whether the approved power state is running.
pub(super) fn wants_running(a: &agent::Assignment) -> bool {
    a.power.as_option().map(|p| p.desired.as_known())
        == Some(Some(pb::PowerState::POWER_STATE_RUNNING))
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Reconciles one assignment and reports what it then observes.
    pub(super) async fn reconcile(&mut self, a: &agent::Assignment) {
        if a.residual {
            return self.residual(a).await;
        }
        let mut t = self.tracked.remove(&a.workload).unwrap_or_default();
        self.refresh(&mut t).await;
        let standing = self.wiring.leases.standing(&a.workload);
        let outcome = match &standing {
            Standing::Held { .. } => self.act(a, &mut t).await,
            Standing::Refused(Refusal::Superseded { by }) => self.superseded(a, &mut t, by).await,
            Standing::Expired { on_expiry: OnExpiry::Stop, .. } => {
                let runs = self.stop_runs(&a.workload, &mut t).await;
                runs.and(self.stop(&mut t).await).map(drop)
            }
            Standing::Expired { on_expiry: OnExpiry::KeepRunning, .. } if has_runs(a) => {
                self.runs(a, &mut t, false).await
            }
            Standing::Expired { on_expiry: OnExpiry::KeepRunning, .. } => {
                self.revive(a, &mut t).await;
                Ok(())
            }
            Standing::Pending | Standing::Refused(_) => Ok(()),
        };
        if let Err(e) = outcome {
            tracing::warn!(workload = %a.workload, error = %e.0, "reconciling failed");
        }
        if matches!(standing, Standing::Held { .. }) {
            t.generation = a.generation;
        }
        self.observe(a, &mut t).await;
        self.tracked.insert(a.workload.clone(), t);
    }

    /// A residual assignment: the steps of its plan, which delete what a retired workload left
    /// here, and nothing else. No lease fences them, because no other node holds what they
    /// touch, and nothing is built, started, or observed (§7 of docs/06-storage.md).
    async fn residual(&mut self, a: &agent::Assignment) {
        let Some(plan) = a.plan.as_option() else { return };
        let mut t = Tracked::default();
        self.steps(a, plan, &mut t).await;
    }

    /// Reads what the runtime object is doing.
    async fn refresh(&self, t: &mut Tracked) {
        let Some(id) = t.runtime.clone() else { return };
        match self.runtime.inspect(&id).await {
            Ok(s) => t.status = Some(s),
            Err(RuntimeError::NotFound(_)) => {
                t.runtime = None;
                t.labels = None;
                t.status = None;
            }
            Err(e) => tracing::warn!(runtime_id = %id, error = %e, "inspect failed"),
        }
        if t.running_since.is_some_and(|s| s.elapsed() >= FORGIVEN) {
            t.restarts = 0;
        }
    }

    /// Under a lease: the plan's steps, then the approved state.
    async fn act(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<(), super::act::Failure> {
        if let Some(plan) = a.plan.as_option()
            && !self.steps(a, plan, t).await
        {
            return Ok(());
        }
        if a.retire.is_set() {
            // Released only once nothing of it runs here any more.
            if !self.stop_runs(&a.workload, t).await? || !self.destroy(t).await? {
                return Ok(());
            }
            if !t.released {
                match self.wiring.leases.release(&a.workload).await {
                    Ok(_) => t.released = true,
                    Err(e) => tracing::error!(error = %e, "the release could not be journalled"),
                }
            }
            return Ok(());
        }
        self.level(a, t).await
    }

    /// The approved state, with no step outstanding: a decision already made, which the node
    /// keeps without asking (§4.6 of docs/02-architecture.md).
    async fn level(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Result<(), super::act::Failure> {
        if has_runs(a) && !wants_running(a) {
            self.stop_runs(&a.workload, t).await?;
            // Retention deletes runs while power is stopped too, and their streams with them.
            let named = a.runs.iter().map(|r| (r.run.as_str(), r)).collect();
            self.drop_unkept(a, t, &named).await;
            return Ok(());
        }
        if has_runs(a) {
            // Its own object is never started: each run is (§4.2 of docs/05-scheduling.md).
            return self.runs(a, t, true).await;
        }
        if !wants_running(a) {
            return self.stop(t).await.map(drop);
        }
        if t.runtime.is_none() {
            // A required dependency not yet running here holds its dependent back (§5 of
            // docs/07-networking.md); it is created once the dependency runs.
            t.waiting = self.waiting_for(a);
            if t.waiting.is_none() {
                self.start(a, t).await?;
            }
            return Ok(());
        }
        if !t.status.as_ref().is_some_and(running) {
            if t.stopped_by_us {
                self.start(a, t).await?;
            } else {
                self.revive(a, t).await;
            }
            return Ok(());
        }
        let cycle = a.power.as_option().map_or(0, |p| p.cycle);
        if self.progress.of(&a.workload).cycle.is_some_and(|done| done < cycle) {
            self.restart(a, t).await?;
        }
        Ok(())
    }

    /// Brings back a workload that exited by itself, waiting longer after each crash.
    async fn revive(&mut self, a: &agent::Assignment, t: &mut Tracked) {
        let crashed = t.runtime.is_some()
            && !t.stopped_by_us
            && !t.status.as_ref().is_some_and(running)
            && wants_running(a);
        if !crashed || t.next_restart.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let exit = t.status.as_ref().and_then(|s| s.exit_code).unwrap_or_default();
        let result = self.start(a, t).await;
        t.restarts = t.restarts.saturating_add(1);
        let wait =
            FIRST.saturating_mul(2u32.saturating_pow(t.restarts.saturating_sub(1))).min(MOST);
        t.next_restart = Some(Instant::now() + wait);
        let mut e = effect(Some(&a.workload), "node.restart", result.is_ok());
        e.after.insert("cause".into(), text("crashed"));
        e.after.insert("exit_code".into(), text(&exit.to_string()));
        self.record(e).await;
    }

    /// Another node holds the workload: it stops here and its data is kept.
    async fn superseded(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        by: &str,
    ) -> Result<(), super::act::Failure> {
        self.stop_runs(&a.workload, t).await?;
        self.stop(t).await?;
        if !t.superseded {
            t.superseded = true;
            let mut e = effect(Some(&a.workload), "node.superseded", true);
            e.after.insert("by".into(), text(by));
            self.record(e).await;
        }
        Ok(())
    }
}
