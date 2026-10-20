// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recording refusals (§4 of docs/15-observability.md), coalesced (§10 of docs/10-api.md): the
//! first refusal of a subject, action, and object is written at once, and its repeats within the
//! window are written as one effect when the window closes, by the next repeat after it or by
//! [`App::close_denial_runs`], whichever comes first. The runs are held in the store, so a
//! restart or a second control plane neither loses nor double-counts them.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, DenialKey, DenialWrite, EffectWrite, RecipeSource, Store, Tx};
use korpis_core::audit::denial::{DenialRun, Offer, offer};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Action, Kind, ObjectRef};
use std::time::SystemTime;

/// How many closed runs one pass of [`App::close_denial_runs`] writes at most.
const CLOSE_BATCH: u32 = 256;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Records that `caller` was refused `action` on `object`.
    pub(crate) async fn record_denial(
        &self,
        caller: &Caller,
        action: &Action,
        object: ObjectRef,
    ) -> Result<(), AppError> {
        let key = DenialKey {
            actor: caller.principal.clone(),
            action: action.as_str().to_owned(),
            object,
        };
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        // A relayed call costs a presentation the relay cannot make, so its refusals cannot
        // flood; each is written at once, naming the relay (§5.2 of docs/08-identity.md).
        if let Some(r) = &caller.relayed {
            tx.append_effect(&denial(&key).relayed_by(Some(r.relay.clone()))).await?;
            tx.commit().await?;
            return Ok(());
        }
        let open = tx.lock_denial_run(&key).await?;
        match offer(open, now, self.denial_window) {
            Offer::Count { run } => tx.put_denial_run(&key, &run).await?,
            Offer::Write { closed, run } => {
                if let Some(effect) = closed.and_then(|c| repeats(&key, &c)) {
                    tx.append_effect(&effect).await?;
                }
                tx.append_effect(&denial(&key)).await?;
                tx.put_denial_run(&key, &run).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Writes the repeats of every run of denials, and of failed attempts to authenticate, whose
    /// window has closed by now, and forgets the runs; returns how many runs it closed. The composition root calls it periodically,
    /// so a run that stops repeating is still written within about one window of its last
    /// repeat.
    ///
    /// # Errors
    /// The store's.
    pub async fn close_denial_runs(&self) -> Result<usize, AppError> {
        let now = self.clock.now();
        let Some(opened_by) = now.checked_sub(self.denial_window) else { return Ok(0) };
        let keys = self.store.denial_runs_opened_by(opened_by, CLOSE_BATCH).await?;
        let mut closed = 0;
        for key in keys {
            if self.close_run(&key, now).await? {
                closed += 1;
            }
        }
        Ok(closed + self.close_failure_runs(now).await?)
    }

    async fn close_run(&self, key: &DenialKey, now: SystemTime) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let Some(run) = tx.lock_denial_run(key).await? else { return Ok(false) };
        if !run.closed(now, self.denial_window) {
            return Ok(false);
        }
        if let Some(effect) = repeats(key, &run) {
            tx.append_effect(&effect).await?;
        }
        tx.drop_denial_run(key).await?;
        tx.commit().await?;
        Ok(true)
    }
}

/// The effect recording one refusal: its subject, and the action that was missing.
fn denial(key: &DenialKey) -> Effect {
    let mut effect = Effect::new(key.actor.clone(), &key.action, Outcome::Denied).error(details([
        ("missing_action", key.action.as_str().into()),
        ("object_kind", key.object.kind.as_str().into()),
        ("object", key.object.id.to_string().into()),
    ]));
    effect.workload = (key.object.kind == Kind::Workload).then_some(key.object.id);
    effect.node = (key.object.kind == Kind::Node).then_some(key.object.id);
    effect
}

/// The effect recording a closed run's repeats, if it counted any: one refusal's record with
/// their count, the first as `occurred_at`, and the last as `window_end`.
fn repeats(key: &DenialKey, run: &DenialRun) -> Option<Effect> {
    let (count, first, last) = run.repeats()?;
    let mut effect = denial(key);
    effect.count = count;
    effect.occurred_at = Some(first);
    effect.window_end = Some(last);
    Some(effect)
}
