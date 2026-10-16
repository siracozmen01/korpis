// SPDX-License-Identifier: AGPL-3.0-or-later

//! Failed attempts to authenticate (§10 of docs/10-api.md). Each is charged to its source's
//! bucket, and answered `RATE_LIMITED` once that bucket refuses, so a client in a loop is told
//! to back off and for how long; and each is recorded as a denial is, the first of a source,
//! kind, and target at once and its repeats within the window as one effect, so a flood of bad
//! credentials cannot become an unbounded write path. A credential that opens is never charged
//! here, whatever its source has done.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite, FailureKey, FailureWrite, RecipeSource, Store, Tx};
use korpis_core::audit::denial::{DenialRun, Offer, offer};
use korpis_core::audit::failure::{Attempt, source};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::principal::Principal;
use korpis_core::rate::Bucket;
use std::net::IpAddr;
use std::time::SystemTime;

/// How many closed runs one pass writes at most.
const CLOSE_BATCH: u32 = 256;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A presentation of `attempt` from `ip`, naming `target`, opened nothing: charges its
    /// source's bucket and records it. The answer to give: `UNAUTHENTICATED`, or `RATE_LIMITED`
    /// once the source's bucket refuses.
    pub async fn authentication_failed(
        &self,
        attempt: Attempt,
        ip: Option<IpAddr>,
        target: Option<ObjectRef>,
    ) -> AppError {
        let key = FailureKey { source: source(ip), attempt, target };
        let bucket = Bucket {
            policy: "failures",
            key: format!("f:{}", key.source),
            rate: self.limiter.rates.failures,
        };
        if let Err(e) = self.record_failure(&key).await {
            return e;
        }
        match self.store.charge(vec![bucket], self.clock.now()).await {
            Ok(standing) => match standing.iter().find(|s| !s.admitted) {
                Some(refused) => {
                    AppError::RateLimited { retry_after: refused.retry_after(), standing }
                }
                None => AppError::Unauthenticated,
            },
            Err(e) => e.into(),
        }
    }

    async fn record_failure(&self, key: &FailureKey) -> Result<(), AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let open = tx.lock_failure_run(key).await?;
        match offer(open, now, self.denial_window) {
            Offer::Count { run } => tx.put_failure_run(key, &run).await?,
            Offer::Write { closed, run } => {
                if let Some(effect) = closed.and_then(|c| repeats(key, &c)) {
                    tx.append_effect(&effect).await?;
                }
                tx.append_effect(&failure(key)).await?;
                tx.put_failure_run(key, &run).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Writes the repeats of every run of failures whose window has closed by `now`, and
    /// forgets the runs; how many it closed.
    pub(crate) async fn close_failure_runs(&self, now: SystemTime) -> Result<usize, AppError> {
        let Some(opened_by) = now.checked_sub(self.denial_window) else { return Ok(0) };
        let keys = self.store.failure_runs_opened_by(opened_by, CLOSE_BATCH).await?;
        let mut closed = 0;
        for key in keys {
            let mut tx = self.store.begin().await?;
            let Some(run) = tx.lock_failure_run(&key).await? else { continue };
            if !run.closed(now, self.denial_window) {
                continue;
            }
            if let Some(effect) = repeats(&key, &run) {
                tx.append_effect(&effect).await?;
            }
            tx.drop_failure_run(&key).await?;
            tx.commit().await?;
            closed += 1;
        }
        Ok(closed)
    }
}

/// The effect recording one failure: by no one, from its source, of its kind, naming its target.
fn failure(key: &FailureKey) -> Effect {
    let target = key.target.map(|t| format!("{}:{}", t.kind.as_str(), t.id)).unwrap_or_default();
    let mut effect = Effect::new(Principal::Anonymous, key.attempt.action(), Outcome::Denied)
        .error(details([
            ("reason", "unauthenticated".into()),
            ("source", key.source.clone().into()),
            ("target", target.into()),
        ]));
    effect.node = key.target.filter(|t| t.kind == Kind::Node).map(|t| t.id);
    effect
}

/// The effect recording a closed run's repeats, if it counted any.
fn repeats(key: &FailureKey, run: &DenialRun) -> Option<Effect> {
    let (count, first, last) = run.repeats()?;
    let mut effect = failure(key);
    effect.count = count;
    effect.occurred_at = Some(first);
    effect.window_end = Some(last);
    Some(effect)
}
