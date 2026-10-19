// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota that counts where workloads run (§5.1 of docs/05-scheduling.md, "Where a workload
//! runs"): an entry naming the nodes it counts holds what each workload it selects runs on those
//! nodes, as its placement's target declares it. It is checked when a workload is bound, by the
//! placement filter, and its totals move at the end of every change to a placement, all under
//! the placement lock, which every one of those changes holds: what the placement was counted as
//! holding comes out of the entries that counted it, exactly, and what it runs now goes into
//! the entries counting the node it is on now.

use super::hold::drift;
use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, Counted, EffectWrite as _, Placed, PlacedQuotaWrite, PlacementWrite as _, QuotaWrite,
    RecipeSource, Store, Tx as _, WorkloadWrite as _,
};
use korpis_core::schedule::Counting;
use korpis_core::tenancy::quota::{Held, QuotaEntry, Totals};
use korpis_core::workload::Workload;
use uuid::Uuid;

/// Reckons `e`, never reckoned, from where every placement under its owner was counted.
async fn reckon_placed<T>(tx: &mut T, e: &mut QuotaEntry) -> Result<(), AppError>
where
    T: PlacedQuotaWrite + QuotaWrite + Send,
{
    let holdings = tx.placed_holdings(e.owner).await?;
    let counted: Vec<_> =
        holdings.iter().filter(|h| e.counts(&h.labels) && e.counts_on(&h.node_labels)).collect();
    let totals = Totals::sum(counted.iter().map(|h| &h.placed.counted.held));
    let workloads: Vec<Uuid> = counted.iter().map(|h| h.workload).collect();
    tx.count_placed_in(e.id, &workloads).await?;
    tx.put_quota_totals(e.id, &totals).await?;
    e.held = Some(totals);
    Ok(())
}

/// What placement checks of `entries` for a workload counted as `placed`: each entry, what it
/// counts beside the workload, and what it counts of it.
#[must_use]
pub(crate) fn counting<'a>(
    entries: &'a [QuotaEntry],
    placed: Option<&'a Placed>,
) -> Vec<Counting<'a>> {
    entries
        .iter()
        .map(|entry| {
            let counted =
                placed.filter(|p| p.counted.by.contains(&entry.id)).map(|p| &p.counted.held);
            let mut rest = entry.held.clone().unwrap_or_default();
            if let Some(held) = counted {
                rest.sub(held);
            }
            Counting { entry, rest, counted }
        })
        .collect()
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The entries naming the nodes they count that select `w`, each reckoned, and where its
    /// placement is counted now. Under the placement lock.
    pub(crate) async fn placed_entries(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
    ) -> Result<(Vec<QuotaEntry>, Option<Placed>), AppError> {
        let chain = self.organization_chain(w.organization).await?;
        let mut entries = tx.placed_quota(w.project, &chain).await?;
        for e in entries.iter_mut().filter(|e| e.held.is_none()) {
            reckon_placed(tx, e).await?;
        }
        entries.retain(|e| e.counts(w.labels.as_map()));
        Ok((entries, tx.placement_held(w.id).await?))
    }

    /// Where `w`'s placement is counted now: the node it is on, what its target runs, and the
    /// entries counting that node; `None` while it is placed nowhere.
    async fn placed_now(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        entries: &[QuotaEntry],
    ) -> Result<Option<Placed>, AppError> {
        let Some(p) = tx.placement_in(w.id).await?.filter(|_| w.deleted_at.is_none()) else {
            return Ok(None);
        };
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {}", w.id));
        let node = self.store.node(p.node).await?.ok_or_else(|| missing("node"))?;
        let target = tx.intent_within(p.intent).await?.ok_or_else(|| missing("placed intent"))?;
        let by = entries.iter().filter(|e| e.counts_on(node.labels.as_map())).map(|e| e.id);
        let counted = Counted { held: Held::of(&target.body), by: by.collect() };
        Ok(Some(Placed { node: p.node, counted }))
    }

    /// Recounts where `w`'s placement is counted, from the placement it has now, moving the
    /// totals of the entries that counted it and of those counting it now by exactly the
    /// difference; whether anything moved. Called under the placement lock at the end of every
    /// change to a placement.
    pub(crate) async fn count_placed(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
    ) -> Result<bool, AppError> {
        let (entries, before) = self.placed_entries(tx, w).await?;
        let after = self.placed_now(tx, w, &entries).await?;
        if before == after {
            return Ok(false);
        }
        let none = Counted::default();
        let was = before.as_ref().map_or(&none, |p| &p.counted);
        let is = after.as_ref().map_or(&none, |p| &p.counted);
        for e in &entries {
            let (counted, counts) = (was.by.contains(&e.id), is.by.contains(&e.id));
            if !counted && !counts {
                continue;
            }
            let mut totals = e.held.clone().unwrap_or_default();
            if counted {
                totals.sub(&was.held);
            }
            if counts {
                totals.add(&is.held);
            }
            tx.put_quota_totals(e.id, &totals).await?;
        }
        tx.put_placement_held(w.id, after.as_ref()).await?;
        Ok(true)
    }

    /// Recounts where `workload`'s placement is counted, under the placement lock, if it is
    /// placed or was counted placed; whether that corrected a difference, which a first count is
    /// not. Part of the reconciliation, in its transaction, after the workload's own recount.
    pub(super) async fn reconcile_placed(
        &self,
        tx: &mut S::Tx,
        workload: Uuid,
    ) -> Result<bool, AppError> {
        let before = tx.placement_held(workload).await?;
        if before.is_none() && tx.placement_in(workload).await?.is_none() {
            return Ok(false);
        }
        tx.lock_placement().await?;
        let Some(w) = tx.lock_workload(workload).await? else { return Ok(false) };
        let before = tx.placement_held(workload).await?.or(before);
        if !self.count_placed(tx, &w).await? || before.is_none() {
            return Ok(false);
        }
        let after = tx.placement_held(workload).await?;
        let (kept, counted) = (format!("{before:?}"), format!("{after:?}"));
        tx.append_effect(&drift(("placement", workload), kept, counted)).await?;
        Ok(true)
    }

    /// Reckons entry `entry`, which names the nodes it counts, if it was never used, or checks
    /// its totals against where the placements it counts were counted, correcting and recording
    /// a difference; whether there was one. Under the placement lock, taken before the entry's.
    pub(super) async fn check_placed_entry(&self, entry: Uuid) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        tx.lock_placement().await?;
        let Some(mut e) = tx.lock_quota_entry(entry).await? else { return Ok(false) };
        let Some(kept) = e.held.clone() else {
            reckon_placed(&mut tx, &mut e).await?;
            tx.commit().await?;
            return Ok(false);
        };
        let counted = Totals::sum(&tx.placed_counted_by(entry).await?);
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
