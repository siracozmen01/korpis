// SPDX-License-Identifier: AGPL-3.0-or-later

//! Making runs (§4 of docs/05-scheduling.md). Every pass, each workload whose runs the control
//! plane tends is read in a transaction of its own, holding the workload and its run state, and
//! what `korpis_core::workload::run::tend` decides is written with an effect for each run made
//! or ended. Two control planes take a workload in turn, so each occurrence is made once.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite as _, LeaseWrite as _, RecipeSource, RunWrite as _, Store, StreamWrite as _,
    Tx, WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::PowerState;
use korpis_core::workload::run::{Run, RunEnd, RunStatus, Tended, Tending, tend};
use korpis_core::workload::stream::{StreamKey, StreamKind};
use std::time::SystemTime;
use uuid::Uuid;

fn when(t: SystemTime) -> String {
    jiff::Timestamp::try_from(t).map_or_else(|_| String::new(), |t| t.to_string())
}

fn effect(workload: Uuid, action: &str, outcome: Outcome) -> Effect {
    let mut e = Effect::new(Principal::System(SystemActor::Reconciler), action, outcome);
    e.workload = Some(workload);
    e
}

/// The effect of making `run`: made, or recorded missed as it was made.
fn made(run: &Run) -> Effect {
    let facts = details([
        ("run", run.id.to_string().into()),
        ("trigger", run.trigger.as_str().into()),
        ("scheduled_for", when(run.scheduled_for).into()),
        ("attempt", run.attempt.into()),
        ("occurrences", run.occurrences.into()),
    ]);
    match (run.status, run.cause) {
        (RunStatus::Missed, Some(cause)) => {
            let mut why = facts;
            why.insert("cause".into(), cause.as_str().into());
            effect(run.workload, "run.missed", Outcome::Failed).error(why)
        }
        _ => effect(run.workload, "run.made", Outcome::Succeeded).after(facts),
    }
}

/// The effect of ending an active run.
fn ended(workload: Uuid, run: Uuid, end: &RunEnd) -> Effect {
    let facts = details([("run", run.to_string().into()), ("cause", end.cause.as_str().into())]);
    match end.status {
        RunStatus::Missed => effect(workload, "run.missed", Outcome::Failed).error(facts),
        _ => effect(workload, "run.cancelled", Outcome::Succeeded).after(facts),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Makes and ends the runs of every workload that has them, as the time, their policies,
    /// and their power say; how many workloads' runs changed.
    ///
    /// # Errors
    /// The store failed.
    pub async fn tend_runs(&self) -> Result<u64, AppError> {
        // While a restore settles, the store's leases and runs are the backup's, and the nodes
        // are presenting what they run (§8 of docs/03-state.md): a run made or lost now could
        // be the second of an occurrence, or a run still running, so none is until it ends.
        if self.settling().await?.is_some() {
            return Ok(0);
        }
        let mut changed = 0;
        for workload in self.store.workloads_with_runs().await? {
            changed += u64::from(self.tend_workload_runs(workload).await?);
        }
        Ok(changed)
    }

    /// One workload's runs, in a transaction of its own; whether anything changed.
    async fn tend_workload_runs(&self, id: Uuid) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let Some(w) = tx.lock_workload(id).await? else { return Ok(false) };
        let state = tx.lock_run_state(id).await?;
        let target = match w.target.filter(|_| w.deleted_at.is_none()) {
            Some(t) => tx.intent_within(t).await?,
            None => None,
        };
        // A workload a suspension holds stopped is tended as stopped: no run is made, and its
        // running ones end (§2.1 of docs/08-identity.md).
        let held = self.held_by(w.organization).await?.is_some();
        let power = self.store.power_at(id, w.power_target_version).await?;
        let zone = target
            .as_ref()
            .and_then(|i| i.body.timezone.as_deref())
            .and_then(|name| self.zones.zone(name));
        let active = tx.active_runs_within(id).await?;
        let newest = tx.newest_run_within(id).await?;
        let lease = tx
            .lease_for_update(id)
            .await?
            .filter(|l| l.released_at.is_none())
            .map(|l| (l.node, l.grantor_expiry()));
        let decided = tend(Tending {
            now: self.clock.now(),
            workload: id,
            target: target.as_ref().map(|i| (i.id, &i.body)),
            power: power
                .map(|p| (if held { PowerState::Stopped } else { p.power.desired }, p.power.cycle)),
            zone: zone.as_ref(),
            state: state.clone(),
            active,
            newest,
            lease,
        });
        let changed = decided.changes(&state);
        if changed {
            self.write_tended(&mut tx, id, &decided).await?;
        }
        // Retention is applied on every pass, not only one that made or ended a run here: a run
        // its node reports ended, or one cancelled, is otherwise kept until something else
        // changes.
        let policy = target.as_ref().map(|i| i.body.run_policy()).unwrap_or_default();
        let pruned = self.prune(&mut tx, id, policy).await?;
        if !changed && !pruned {
            return Ok(false);
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Deletes the runs of `workload` that `policy` no longer keeps; whether it deleted any. A
    /// run retention deletes takes its console stream with it (§4 of docs/14-streams.md): its
    /// node deletes the volume once the run is neither named nor kept, and the stream's
    /// offloaded segments are kept no longer than the operator's floor.
    async fn prune(
        &self,
        tx: &mut S::Tx,
        workload: Uuid,
        policy: korpis_core::workload::spec::RunPolicy,
    ) -> Result<bool, AppError> {
        let now = self.clock.now();
        let pruned = tx.prune_runs(workload, policy.keep_successful, policy.keep_failed).await?;
        for run in &pruned {
            let key = StreamKey { workload, kind: StreamKind::Console, run: Some(*run) };
            tx.end_stream(key, now).await?;
        }
        Ok(!pruned.is_empty())
    }

    async fn write_tended(
        &self,
        tx: &mut S::Tx,
        workload: Uuid,
        decided: &Tended,
    ) -> Result<(), AppError> {
        for (run, end) in &decided.ended {
            if tx.end_run(*run, *end).await? {
                tx.append_effect(&ended(workload, *run, end)).await?;
            }
        }
        for run in &decided.made {
            if tx.insert_run(run).await? {
                tx.append_effect(&made(run)).await?;
            }
        }
        tx.set_run_state(&decided.state).await?;
        Ok(())
    }
}
