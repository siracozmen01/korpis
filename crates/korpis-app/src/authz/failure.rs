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
use korpis_core::rate::{Bucket, Standing};
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

/// How many closed runs one pass writes at most.
const CLOSE_BATCH: u32 = 256;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A presentation of `attempt` from `ip`, naming `target`, opened nothing: charges its
    /// source's bucket and records it. The answer to give: `UNAUTHENTICATED`, or `RATE_LIMITED`
    /// once the source's bucket refuses.
    ///
    /// What is recorded is bounded whoever sends the failures, and no sender can hide another's.
    /// A target is kept only if it exists, so it is always one of the installation's nodes. A
    /// failure joining a run already open costs nothing more. One that would open a run, and so
    /// write an effect at once, does so under its own source only while its source's bucket
    /// admits it and the bucket the whole installation shares has room, which it then spends;
    /// otherwise it is counted in the run of its kind and target under the source `*`. One
    /// address can therefore spend no more of the shared bucket than its own allows, the log
    /// receives at most that bucket's worth of effects and one per kind and node each window, and
    /// a failure against a node is recorded against that node whatever the budgets say.
    pub async fn authentication_failed(
        &self,
        attempt: Attempt,
        ip: Option<IpAddr>,
        target: Option<ObjectRef>,
    ) -> AppError {
        match self.charge_failure(attempt, ip, target).await {
            Ok(answer) | Err(answer) => answer,
        }
    }

    async fn charge_failure(
        &self,
        attempt: Attempt,
        ip: Option<IpAddr>,
        target: Option<ObjectRef>,
    ) -> Result<AppError, AppError> {
        let source = source(ip);
        let rate = self.limiter.rates.failures;
        let own = Bucket { policy: "failures", key: format!("f:{source}"), rate };
        let standing = self.store.charge(vec![own], self.clock.now()).await?;
        let refused = standing.iter().find(|s| !s.admitted).map(Standing::retry_after);
        let key = FailureKey { source, attempt, target: self.existing(target).await? };
        self.record_failure(&key, refused.is_none()).await?;
        Ok(match refused {
            Some(retry_after) => AppError::RateLimited { retry_after, standing },
            None => AppError::Unauthenticated,
        })
    }

    /// `target`, if it names something that exists; only a node is ever named.
    async fn existing(&self, target: Option<ObjectRef>) -> Result<Option<ObjectRef>, AppError> {
        let Some(t) = target.filter(|t| t.kind == Kind::Node) else { return Ok(None) };
        Ok(self.store.node(t.id).await?.map(|_| t))
    }

    /// Whether the bucket the installation shares has room for a run recorded apart, which
    /// it then spends.
    async fn recordable(&self, now: SystemTime) -> Result<bool, AppError> {
        let rate = self.limiter.rates.recorded;
        let all = Bucket { policy: "failures-recorded", key: "f:*".into(), rate };
        Ok(self.store.charge(vec![all], now).await?.iter().all(|s| s.admitted))
    }

    /// Counts the failure `key` names in its run, or opens one; a run is opened under its own
    /// source only `within` its source's budget and with room in the installation's.
    async fn record_failure(&self, key: &FailureKey, within: bool) -> Result<(), AppError> {
        let (now, window) = (self.clock.now(), self.denial_window);
        let mut tx = self.store.begin().await?;
        let offered = offer(tx.lock_failure_run(key).await?, now, window);
        let apart = match offered {
            Offer::Count { .. } => true,
            Offer::Write { .. } => within && self.recordable(now).await?,
        };
        if apart {
            enter(&mut tx, key, offered).await?;
        } else {
            together(&mut tx, key, offered, (now, window)).await?;
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

/// Counts a failure that may not open a run of its own in the run of its kind and target under
/// the source `*`, closing its own run first if `offered` found that closed.
async fn together<T: FailureWrite + EffectWrite + Send>(
    tx: &mut T,
    key: &FailureKey,
    offered: Offer,
    (now, window): (SystemTime, Duration),
) -> Result<(), AppError> {
    if let Offer::Write { closed: Some(c), .. } = offered {
        if let Some(effect) = repeats(key, &c) {
            tx.append_effect(&effect).await?;
        }
        tx.drop_failure_run(key).await?;
    }
    let together = FailureKey { source: "*".into(), ..key.clone() };
    let offered = offer(tx.lock_failure_run(&together).await?, now, window);
    enter(tx, &together, offered).await
}

/// Enters a failure in the run of `key` as `offered`: counted in it, or written at once after
/// the closed run's repeats, opening a new one.
async fn enter<T: FailureWrite + EffectWrite + Send>(
    tx: &mut T,
    key: &FailureKey,
    offered: Offer,
) -> Result<(), AppError> {
    match offered {
        Offer::Count { run } => tx.put_failure_run(key, &run).await?,
        Offer::Write { closed, run } => {
            if let Some(effect) = closed.and_then(|c| repeats(key, &c)) {
                tx.append_effect(&effect).await?;
            }
            tx.append_effect(&failure(key)).await?;
            tx.put_failure_run(key, &run).await?;
        }
    }
    Ok(())
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
