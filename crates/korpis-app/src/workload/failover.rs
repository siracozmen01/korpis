// SPDX-License-Identifier: AGPL-3.0-or-later

//! Failover (§2.4 of docs/05-scheduling.md): a workload placed on a node the silence sweep marked
//! unreachable is placed again, by the filter and the score as any placement is, once the
//! grantor's end of its lease has passed, when it stops where its lease ends and another node
//! reaches all of its data. Nothing observes the old node: the lease is what says it stopped
//! there, since its holder stops a `stop` workload by its own clock before that end (§4.5 of
//! docs/02-architecture.md). One that no node can take keeps its placement, and the attempt is
//! recorded for its status and tried again at the next pass; it is forgotten once the node is
//! heard again. Where it lands it is made by a remake plan (`super::remake`).

use super::remake::Held;
use super::screen::Screened;
use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{
    Clock, LeaseWrite as _, PlacementWrite as _, RecipeSource, Store, Tx as _, WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::node::Connectivity;
use korpis_core::placement::Placement;
use korpis_core::workload::Workload;
use korpis_core::workload::spec::Continuity;
use uuid::Uuid;

/// A workload placed again by failover: the node it leaves.
pub(super) struct Failover {
    /// The unreachable node.
    pub(super) from: Uuid,
}

impl Failover {
    /// The effect of `w`'s failover to `placed`, taking the volumes in `held`, under `grant`.
    pub(super) fn effect(
        &self,
        w: &Workload,
        placed: &Placement,
        (held, grant): (&[Held], Option<Uuid>),
    ) -> Effect {
        let actor = korpis_core::principal::Principal::System(
            korpis_core::principal::SystemActor::Scheduler,
        );
        let volumes: Vec<String> = held.iter().map(|h| h.id.to_string()).collect();
        let mut e = Effect::new(actor, "workload.failover", Outcome::Succeeded).after(details([
            ("from", self.from.to_string().into()),
            ("to", placed.node.to_string().into()),
            ("intent", placed.intent.to_string().into()),
            ("volumes", volumes.join(", ").into()),
            ("reason", placed.reason.clone().into()),
        ]));
        e.workload = Some(w.id);
        e.grant = grant;
        e
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One pass over every workload placed on an unreachable node: each that may be placed
    /// again now is, in a transaction of its own, after the attempts for workloads whose node
    /// was heard again are forgotten. Returns how many moved.
    pub async fn fail_over(&self) -> Result<u64, AppError> {
        let mut tx = self.store.begin().await?;
        tx.forget_returned().await?;
        tx.commit().await?;
        let mut page = Page::new(None, Page::DEFAULT);
        let mut moved = 0;
        loop {
            let ids = self.store.placed_unreachable(page).await?;
            for id in &ids {
                moved += u64::from(self.fail_over_one(*id).await?);
            }
            match page.next(&ids, |id| *id) {
                Some(after) => page = Page::new(Some(after), page.size),
                None => return Ok(moved),
            }
        }
    }

    /// Places `id` again if it may be now; whether it moved. It is left where it is while its
    /// lease is not over at the grantor's end, when it keeps running where its lease ends, and
    /// while a plan of it is under way, or it was never made: that plan's steps are the old
    /// node's to finish, or an operator's to cancel.
    async fn fail_over_one(&self, id: Uuid) -> Result<bool, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Some(w) = tx.lock_workload(id).await? else { return Ok(false) };
        let Some(t) = w.target.filter(|_| w.deleted_at.is_none()) else { return Ok(false) };
        if tx.lease_for_update(id).await?.is_some_and(|l| !l.over(now)) {
            return Ok(false);
        }
        tx.lock_placement().await?;
        let Some(current) = tx.placement_in(id).await? else { return Ok(false) };
        let Some(target) = tx.intent_within(t).await?.filter(|i| i.id == current.intent) else {
            return Ok(false);
        };
        if !tx.made_and_settled(id).await? {
            return Ok(false);
        }
        let from = current.node;
        let gone = self.store.node(from).await?.map(|n| n.connectivity);
        if current.held_at.is_some() || gone != Some(Connectivity::Unreachable) {
            return Ok(false);
        }
        if self.on_expiry(from, &target.body).await? != Continuity::Stop {
            return Ok(false);
        }
        let failover = Failover { from };
        let screened = Screened::default();
        self.place_new(&mut tx, (&w, &target), None, &screened, Some(&failover)).await?;
        let moved = tx.placement_in(id).await?.is_some_and(|p| p.node != from);
        if moved {
            self.count_placed(&mut tx, &w).await?;
        }
        // A failover that found no node is committed too: its attempt is what the status shows.
        tx.commit().await?;
        Ok(moved)
    }
}
