// SPDX-License-Identifier: AGPL-3.0-or-later

//! The runs a restore recovers from what nodes present (§8 of docs/03-state.md): each run an
//! assignment names that the store lacks is recorded as its signed reference made it, running on
//! the node that presented it, and one the store holds as pending is started there; what the
//! store keeps of the workload's schedule and power cycles is moved past them, so no second run
//! is made for what is already running. A run is recovered only where its workload is placed, as
//! of another workload never, and as one the store already went past (from an assignment older
//! than what it holds) never. A run the store holds as active that no node presents is
//! left to the run maker, which settles it as it settles any other once the window ends.

use super::recover::{effect, with};
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, Opened, RecipeSource, RunWrite as _, Sealed, Store, WorkloadWrite as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::workload::run::{RunRef, RunState, RunStatus, past, recovered, settled};
use std::time::SystemTime;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The runs `opened` names, recovered in `tx`, which holds its workload's lock.
    pub(super) async fn recover_runs(
        &self,
        tx: &mut S::Tx,
        opened: &Opened,
        sealed: &Sealed,
    ) -> Result<Vec<Effect>, AppError> {
        if opened.runs.is_empty() {
            return Ok(Vec::new());
        }
        // Locked first, so what the store went past is read as recovery then moves it.
        let state = tx.lock_run_state(opened.workload).await?;
        let newest = tx.newest_run_within(opened.workload).await?.map(|r| r.scheduled_for);
        let mut effects = Vec::new();
        for r in &opened.runs {
            effects.extend(self.recover_run(tx, (opened, sealed), r, (&state, newest)).await?);
        }
        let body = &opened.target.body;
        let zone = body.timezone.as_deref().and_then(|name| self.zones.zone(name));
        let moved = past(state.clone(), &opened.runs, (opened.target.id, body), zone.as_ref());
        if moved != state {
            tx.set_run_state(&moved).await?;
        }
        Ok(effects)
    }

    /// One run `opened` names: refused when it is another workload's, started where the store
    /// holds it pending, left where it holds it otherwise, and, where it lacks it, recorded
    /// unless the store already went past it, as it went past a run from an older assignment,
    /// or another run of the workload holds its occurrence or power cycle.
    async fn recover_run(
        &self,
        tx: &mut S::Tx,
        (opened, sealed): (&Opened, &Sealed),
        r: &RunRef,
        (state, newest): (&RunState, Option<SystemTime>),
    ) -> Result<Option<Effect>, AppError> {
        let about = |action, outcome, mut more: serde_json::Map<String, serde_json::Value>| {
            let run = korpis_core::id::Id::from_uuid(korpis_core::id::IdKind::Run, r.run);
            more.insert("run".into(), run.to_string().into());
            with(effect(action, outcome, opened, sealed), more)
        };
        let refused = |why: &str| {
            Some(about("restore.run.refused", Outcome::Failed, details([("why", why.into())])))
        };
        let now = self.clock.now();
        match self.store.run(r.run).await? {
            Some(held) if held.workload != opened.workload => return Ok(refused("workload")),
            Some(held) if held.status != RunStatus::Pending => return Ok(None),
            Some(_) => {
                let started = tx.start_run(r.run, opened.node, now).await?;
                let e = about("restore.run.started", Outcome::Succeeded, details([]));
                return Ok(started.then_some(e));
            }
            None if settled(state, r, newest) => return Ok(refused("settled")),
            None => {}
        }
        // The intent it was made under is the target, recovered just before, or one the store
        // still holds; an older one the backup also lacks is lost, and the target stands in.
        let held = r.intent == opened.target.id || tx.intent_within(r.intent).await?.is_some();
        let intent = if held { r.intent } else { opened.target.id };
        let run = recovered(r, opened.workload, intent, opened.node, now);
        if tx.insert_run(&run).await? {
            let more = details([("intent_substituted", (!held).into())]);
            return Ok(Some(about("restore.run.recovered", Outcome::Succeeded, more)));
        }
        Ok(refused("occupied"))
    }
}
