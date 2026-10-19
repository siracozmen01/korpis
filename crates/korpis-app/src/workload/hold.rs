// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each workload holds against quota, kept beside every entry as its totals (question 7 of
//! §10 of docs/05-scheduling.md). A workload's holding is recounted in the transaction of every
//! change to it (a declaration, its target moving, its retirement): what it was counted as
//! holding is taken back out of the entries that counted it and what it holds now is added to
//! the entries counting it now, so the totals move by exactly what they held of it. An entry is
//! reckoned from its workloads' holdings the first time it is used. A reconciliation recounts
//! every workload from its intents and checks every entry's totals against its workloads, and
//! records each difference it corrects rather than trusting the totals.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, Counted, EffectWrite as _, QuotaWrite, RecipeSource, Store, Tx as _, WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::tenancy::quota::{Held, QuotaEntry, Totals};
use korpis_core::workload::Workload;
use uuid::Uuid;

/// What a recount found.
enum Recount {
    /// Nothing differed.
    Same,
    /// The workload was counted for the first time.
    First,
    /// What it was counted as holding differed from what its intents say.
    Moved,
}

fn drift(subject: (&str, Uuid), kept: String, counted: String) -> Effect {
    Effect::new(Principal::System(SystemActor::Reconciler), "quota.drift", Outcome::Succeeded)
        .after(details([
            (subject.0, subject.1.to_string().into()),
            ("kept", kept.into()),
            ("counted", counted.into()),
        ]))
}

/// Reckons `e`, locked and never reckoned, from what every workload under its owner was counted
/// as holding, and counts it by the workloads it selects.
async fn reckon<T: QuotaWrite>(tx: &mut T, e: &mut QuotaEntry) -> Result<(), AppError> {
    let holdings = tx.holdings(e.owner).await?;
    let counted: Vec<_> = holdings.iter().filter(|h| e.counts(&h.labels)).collect();
    let totals = Totals::sum(counted.iter().map(|h| &h.counted.held));
    let workloads: Vec<Uuid> = counted.iter().map(|h| h.workload).collect();
    tx.count_in(e.id, &workloads).await?;
    tx.put_quota_totals(e.id, &totals).await?;
    e.held = Some(totals);
    Ok(())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What `w` holds now: the larger of what its head declares and its target runs; nothing
    /// once it is deleted.
    async fn held_now(&self, tx: &mut S::Tx, w: &Workload) -> Result<Option<Held>, AppError> {
        if w.deleted_at.is_some() {
            return Ok(None);
        }
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {}", w.id));
        let declared = tx.intent_within(w.intent).await?.ok_or_else(|| missing("head"))?;
        let mut held = Held::of(&declared.body);
        if let Some(t) = w.target {
            let target = tx.intent_within(t).await?.ok_or_else(|| missing("target"))?;
            held = held.max(&Held::of(&target.body));
        }
        Ok(Some(held))
    }

    /// The quota entries of `project` and of `organization` and every organization above it,
    /// locked, each with its totals, reckoning any not yet reckoned.
    pub(super) async fn locked_entries(
        &self,
        tx: &mut S::Tx,
        project: Uuid,
        organization: Uuid,
    ) -> Result<Vec<QuotaEntry>, AppError> {
        let chain = self.organization_chain(organization).await?;
        let mut entries = tx.lock_quota(project, &chain).await?;
        for e in entries.iter_mut().filter(|e| e.held.is_none()) {
            reckon(tx, e).await?;
        }
        Ok(entries)
    }

    /// Recounts what `workload` holds, in `tx`, moving the totals of the entries that counted it
    /// and of those that count it now by exactly the difference. Called in the transaction of
    /// every change to what it holds.
    pub(crate) async fn rehold(&self, tx: &mut S::Tx, workload: Uuid) -> Result<(), AppError> {
        self.recount(tx, workload).await.map(|_| ())
    }

    async fn recount(&self, tx: &mut S::Tx, workload: Uuid) -> Result<Recount, AppError> {
        let Some(w) = tx.lock_workload(workload).await? else { return Ok(Recount::Same) };
        let now = self.held_now(tx, &w).await?;
        let entries = self.locked_entries(tx, w.project, w.organization).await?;
        let before = tx.counted(w.id).await?;
        let after = match now {
            None => Counted::default(),
            Some(held) => {
                let labels = w.labels.as_map();
                let by = entries.iter().filter(|e| e.counts(labels)).map(|e| e.id).collect();
                Counted { held, by }
            }
        };
        let was = before.clone().unwrap_or_default();
        if before.as_ref() == Some(&after) {
            return Ok(Recount::Same);
        }
        for e in &entries {
            let (counted, counts) = (was.by.contains(&e.id), after.by.contains(&e.id));
            if !counted && !counts {
                continue;
            }
            let mut totals = e.held.clone().unwrap_or_default();
            if counted {
                totals.sub(&was.held);
            }
            if counts {
                totals.add(&after.held);
            }
            tx.put_quota_totals(e.id, &totals).await?;
        }
        tx.put_counted(w.id, &after).await?;
        Ok(if before.is_none() { Recount::First } else { Recount::Moved })
    }

    /// Recounts every workload from its intents, then reckons every entry not yet reckoned and
    /// checks every other's totals against what its workloads are counted as holding; each
    /// difference corrected is recorded as a
    /// `quota.drift` effect, since nothing but a change that skipped its recount makes one. The
    /// first count of a workload is not a difference. How many were corrected.
    ///
    /// # Errors
    /// The store failed.
    pub async fn reconcile_quota(&self) -> Result<u64, AppError> {
        let mut corrected = 0;
        for workload in self.store.workloads_to_count().await? {
            let mut tx = self.store.begin().await?;
            let before = tx.counted(workload).await?;
            if let Recount::Moved = self.recount(&mut tx, workload).await? {
                let after = tx.counted(workload).await?.unwrap_or_default();
                let kept = format!("{:?}", before.unwrap_or_default().held);
                tx.append_effect(&drift(("workload", workload), kept, format!("{:?}", after.held)))
                    .await?;
                corrected += 1;
            }
            tx.commit().await?;
        }
        for entry in self.store.quota_entry_ids().await? {
            corrected += u64::from(self.check_entry(entry).await?);
        }
        Ok(corrected)
    }

    /// Reckons entry `entry` if it was never used, which is its first count and no difference,
    /// or checks its totals against what its workloads are counted as holding, correcting and
    /// recording a difference; whether there was one.
    async fn check_entry(&self, entry: Uuid) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let Some(mut e) = tx.lock_quota_entry(entry).await? else { return Ok(false) };
        let Some(kept) = e.held.clone() else {
            reckon(&mut tx, &mut e).await?;
            tx.commit().await?;
            return Ok(false);
        };
        let counted = Totals::sum(&tx.counted_by(entry).await?);
        let differs = counted != kept;
        if differs {
            tx.put_quota_totals(entry, &counted).await?;
            let (kept, counted) = (format!("{kept:?}"), format!("{counted:?}"));
            tx.append_effect(&drift(("entry", entry), kept, counted)).await?;
        }
        tx.commit().await?;
        Ok(differs)
    }
}
