// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's convergence as a condition event (§5 of docs/15-observability.md). Convergence is
//! derived when it is read, from the target, the plans, the observation, and the clock, so no
//! single transaction changes it; an observation going stale changes it without any write at
//! all. So the control plane decides it here, on every announced change and on every resync,
//! records what it decided, and writes `convergence.changed` when that differs from the last
//! decision. Several processes may decide at once; the record is swapped by compare-and-swap, so
//! a change is written once.

use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, ConditionWrite, RecipeSource, Store, Tx, WorkloadFilter};
use korpis_core::event::{Condition, ConditionChange};
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Decides a workload's convergence now; whether that wrote a `convergence.changed` event.
    /// The first decision about a workload is recorded without one: there is no change to report.
    pub async fn track_convergence(&self, workload: Uuid) -> Result<bool, AppError> {
        let Some(w) = self.store.workload(workload).await?.filter(|w| w.deleted_at.is_none())
        else {
            return Ok(false);
        };
        let target = match w.target {
            Some(t) => self.store.intent(t).await?,
            None => None,
        };
        let open = self.store.open_plans(w.id).await?;
        let found = self.found(&w, target.as_ref()).await?;
        let placed = found.placed_for(w.target);
        let (now_is, _) = self.measure(&w, &open, found.node(), placed).await?;
        let decided = self.store.decided_convergence(w.id).await?;
        if decided == Some(now_is) {
            return Ok(false);
        }
        let at = self.clock.now();
        let mut tx = self.store.begin().await?;
        if !tx.swap_convergence(w.id, decided, now_is, at).await? {
            return Ok(false);
        }
        let Some(from) = decided else {
            tx.commit().await?;
            return Ok(false);
        };
        let change = ConditionChange::Convergence { from, to: now_is };
        tx.append_condition(&Condition { workload: w.id, at, change }).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Decides every live workload's convergence: the pass that catches what nothing announces.
    /// How many changes it wrote.
    pub async fn track_all_convergence(&self) -> Result<u64, AppError> {
        let mut page = Page::new(None, Page::MAX);
        let mut written = 0;
        loop {
            let found = self.store.workloads(&WorkloadFilter::default(), page).await?;
            for w in &found {
                written += u64::from(self.track_convergence(w.id).await?);
            }
            match page.next(&found, |w| w.id) {
                Some(after) => page = Page::new(Some(after), Page::MAX),
                None => return Ok(written),
            }
        }
    }
}
