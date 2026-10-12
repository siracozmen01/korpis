// SPDX-License-Identifier: AGPL-3.0-or-later

//! Binding a workload's approved target to a node (§2.3 of docs/05-scheduling.md), in the
//! transaction that approved it and under the placement lock.
//!
//! A workload not yet placed is placed wherever the policy prefers. A workload already placed
//! follows its new target in place if its node can hold it, and is otherwise unplaced for that
//! target: moving it is a migration (§8 of docs/05-scheduling.md), never a side effect of an
//! approval. Every attempt that finds no node is kept with its full derivation and logged once
//! per target, so retrying does not flood the log.

use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, EffectWrite, PlacementWrite, RecipeSource, Store, Tx, WorkloadWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::placement::{BoundBy, Placement, Unplaced};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::schedule::{Candidate, Current, Explanation, Request, place, reservation};
use korpis_core::tenancy::quota::Bounds;
use korpis_core::workload::{Intent, Workload};
use uuid::Uuid;

/// What a bind writes, beside its explanation.
struct Binding<'a> {
    w: &'a Workload,
    target: &'a Intent,
    current: Option<Placement>,
    node: Uuid,
}

fn scheduler() -> Principal {
    Principal::System(SystemActor::Scheduler)
}

fn place_effect(w: &Workload, outcome: Outcome) -> Effect {
    let mut e = Effect::new(scheduler(), "workload.place", outcome);
    e.workload = Some(w.id);
    e
}

/// A placement's reason, for a human. It names no count and no node: every reader of the
/// workload sees it, and the explanation carries what only some may see.
fn reason(followed: bool) -> String {
    if followed {
        "its target changed and still fits on the node it is on".to_owned()
    } else {
        "the best scored node able to run it, by the installation's policy".to_owned()
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organizations whose nodes may serve one owned by `organization`: it and every
    /// ancestor (§3.4 of docs/01-model.md).
    async fn owners(&self, organization: Uuid) -> Result<Vec<Uuid>, AppError> {
        let chain = self.store.ancestry(ObjectRef::new(Kind::Organization, organization)).await?;
        Ok(chain.into_iter().map(|n| n.object.id).collect())
    }

    /// One pass over every workload whose approved target has no node: each is tried again, in
    /// a transaction of its own, against the nodes as they are now. What a cordon lifted, a
    /// node enrolled, or a workload deleted freed is found here. Returns how many were tried.
    pub async fn reconcile_placements(&self) -> Result<u64, AppError> {
        let mut page = Page::new(None, Page::DEFAULT);
        let mut tried = 0;
        loop {
            let ids = self.store.awaiting_placement(page).await?;
            for id in &ids {
                tried += u64::from(self.retry_placement(*id).await?);
            }
            match page.next(&ids, |id| *id) {
                Some(after) => page = Page::new(Some(after), page.size),
                None => return Ok(tried),
            }
        }
    }

    /// Tries one workload again, in a transaction of its own; false if it has no target now.
    async fn retry_placement(&self, id: Uuid) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let Some(w) = tx.lock_workload(id).await? else { return Ok(false) };
        let Some(t) = w.target.filter(|_| w.deleted_at.is_none()) else { return Ok(false) };
        let target = self.store.intent(t).await?;
        self.place_target(&mut tx, &w, target.as_ref()).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Makes the workload's placement follow its approved target: places, follows in place,
    /// records that no node can take it, or, with no target, removes it.
    pub(crate) async fn place_target(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        target: Option<&Intent>,
    ) -> Result<(), AppError> {
        tx.lock_placement().await?;
        let current = tx.placement_in(w.id).await?;
        let Some(target) = target else {
            if current.is_some() {
                tx.unbind(w.id).await?;
                tx.append_effect(&place_effect(w, Outcome::RolledBack)).await?;
            }
            return Ok(());
        };
        if current.as_ref().is_some_and(|p| p.intent == target.id) {
            // A cancellation put back the target it is placed for; a failure recorded for the
            // target that was withdrawn no longer says anything.
            tx.clear_unplaced(w.id).await?;
            return Ok(());
        }
        let owners = self.owners(w.organization).await?;
        let mut nodes = tx.serving_nodes(&owners).await?;
        if let Some(p) = &current {
            nodes.retain(|n| n.id == p.node);
        }
        let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
        let allocations = tx.allocations(&ids).await?;
        let peers = tx.peers(w.organization, w.id).await?;
        let empty = Bounds::default();
        let candidates: Vec<Candidate<'_>> = nodes
            .iter()
            .map(|n| Candidate { node: n, allocated: allocations.get(&n.id).unwrap_or(&empty) })
            .collect();
        let req = Request {
            workload: w.id,
            project: w.project,
            labels: w.labels.as_map(),
            owners: &owners,
            body: &target.body,
            current: current.as_ref().map(|p| Current { node: p.node, reserved: &p.reserved }),
        };
        let explanation = place(&req, &candidates, &peers, &self.policy);
        match explanation.chosen {
            Some(node) => {
                let binding = Binding { w, target, current, node };
                self.bind_chosen(tx, binding, explanation).await
            }
            None => self.record_unplaced(tx, w, target, explanation).await,
        }
    }

    async fn bind_chosen(
        &self,
        tx: &mut S::Tx,
        binding: Binding<'_>,
        explanation: Explanation,
    ) -> Result<(), AppError> {
        let Binding { w, target, current, node } = binding;
        let followed = current.is_some();
        let (bound_at, bound_by) =
            current.map_or((self.clock.now(), BoundBy::Scheduler), |p| (p.bound_at, p.bound_by));
        let placement = Placement {
            workload: w.id,
            node,
            intent: target.id,
            reserved: reservation(&target.body),
            bound_at,
            bound_by,
            reason: reason(followed),
            explanation,
            authoritative: true,
        };
        tx.bind(&placement).await?;
        tx.clear_unplaced(w.id).await?;
        let mut e = place_effect(w, Outcome::Succeeded).after(details([
            ("node", node.to_string().into()),
            ("intent", target.id.to_string().into()),
            ("followed", followed.into()),
            ("reason", placement.reason.clone().into()),
        ]));
        e.node = Some(node);
        tx.append_effect(&e).await?;
        Ok(())
    }

    async fn record_unplaced(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        target: &Intent,
        explanation: Explanation,
    ) -> Result<(), AppError> {
        let (candidates, rejections) = (explanation.candidates, explanation.filtered.len());
        let unplaced =
            Unplaced { workload: w.id, intent: target.id, since: self.clock.now(), explanation };
        if tx.record_unplaced(&unplaced).await? {
            let e = place_effect(w, Outcome::Failed).error(details([
                ("reason", "no node can run it".into()),
                ("intent", target.id.to_string().into()),
                ("candidates", candidates.into()),
                ("rejections", rejections.into()),
            ]));
            tx.append_effect(&e).await?;
        }
        Ok(())
    }
}
