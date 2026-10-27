// SPDX-License-Identifier: AGPL-3.0-or-later

//! Binding a workload's approved target to a node (§2.3 of docs/05-scheduling.md), in the
//! transaction that approved it and under the placement lock.
//!
//! A workload not yet placed is placed wherever the policy prefers. A workload already placed
//! follows its new target in place if its node can hold it, and is otherwise unplaced for that
//! target: moving it is a migration (§8 of docs/05-scheduling.md), never a side effect of an
//! approval. Every attempt that finds no node is kept with its full derivation and logged once
//! per target, so retrying does not flood the log.

use super::failover::Failover;
use super::remake::Held;
use super::screen::Screened;
use crate::App;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, EffectWrite, PlacementWrite, RecipeSource, Store, Tx, WorkloadWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::node::Node;
use korpis_core::node::device::DeviceHold;
use korpis_core::placement::{BoundBy, Placement, Unplaced};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::schedule::{Candidate, Current, Explanation, Request, place};
use korpis_core::tenancy::quota::Bounds;
use korpis_core::workload::volume::Home;
use korpis_core::workload::{Intent, Workload};
use uuid::Uuid;

/// What a bind writes, beside its explanation.
struct Binding<'a> {
    w: &'a Workload,
    target: &'a Intent,
    current: Option<Placement>,
    node: Uuid,
    /// The devices and cores it is given there, as the filter found them free, and what it
    /// reserves holding them; computed before `current` is moved in, since the request
    /// computing them borrows it.
    given: Given,
    /// The node it leaves, when it is placed again by failover.
    failover: Option<&'a Failover>,
    /// The volumes it holds, when it was made on another node and is made here by a remake
    /// plan (`super::remake`).
    remade: Option<Vec<Held>>,
}

/// What a workload is given on the node it is bound to.
pub(super) struct Given {
    /// Its devices (§6.3 of docs/05-scheduling.md).
    pub(super) devices: Vec<DeviceHold>,
    /// Its cores (§5.2 of docs/05-scheduling.md).
    pub(super) cores: Vec<u32>,
    /// What it reserves there: its cores' threads, where it holds any, for its declared CPU.
    pub(super) reserved: Bounds,
}

/// `nodes` as candidates, each with what its placements reserve: `allocations`, or `empty`.
pub(super) fn candidates_of<'a>(
    nodes: &'a [Node],
    allocations: &'a std::collections::BTreeMap<Uuid, Bounds>,
    empty: &'a Bounds,
) -> Vec<Candidate<'a>> {
    nodes
        .iter()
        .map(|n| Candidate { node: n, allocated: allocations.get(&n.id).unwrap_or(empty) })
        .collect()
}

/// Where `p` places its workload now, as a placement that may follow its target in place.
fn current_of(p: &Placement) -> Current<'_> {
    Current { node: p.node, reserved: &p.reserved, devices: &p.devices, cores: &p.cores }
}

/// The devices and cores `req` is given on `node`, one of `nodes`, and what it reserves holding
/// them: what the filter found free there, so failing now is the store and the scheduler
/// disagreeing.
pub(super) fn given_on(req: &Request<'_>, nodes: &[Node], node: Uuid) -> Result<Given, AppError> {
    let disagree = |why: String| AppError::Internal(format!("the chosen node's holds: {why}"));
    let n =
        nodes.iter().find(|n| n.id == node).ok_or_else(|| disagree("not a candidate".into()))?;
    let devices = korpis_core::schedule::devices::given(req, n).map_err(disagree)?;
    let cores = korpis_core::schedule::cores::given(req, n).map_err(disagree)?;
    let reserved =
        korpis_core::schedule::cores::reservation_on((req.body, req.edge_tier), n, &cores);
    Ok(Given { devices, cores, reserved })
}

fn scheduler() -> Principal {
    Principal::System(SystemActor::Scheduler)
}

/// A placement effect, naming the grant of the decision it carries out.
pub(super) fn place_effect(w: &Workload, outcome: Outcome, grant: Option<Uuid>) -> Effect {
    let mut e = Effect::new(scheduler(), "workload.place", outcome);
    e.workload = Some(w.id);
    e.grant = grant;
    e
}

/// A placement's reason, for a human. It names no count and no node: every reader of the
/// workload sees it, and the explanation carries what only some may see.
fn reason(followed: bool, failover: bool) -> String {
    if failover {
        "its node went silent and its lease ended; the best scored node reaching its data"
            .to_owned()
    } else if followed {
        "its target changed and still fits on the node it is on".to_owned()
    } else {
        "the best scored node able to run it, by the installation's policy".to_owned()
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organizations whose nodes may serve one owned by `organization`: it and every
    /// ancestor, nearest first (§3.4 of docs/01-model.md).
    pub(crate) async fn owners(&self, organization: Uuid) -> Result<Vec<Uuid>, AppError> {
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

    /// Tries one workload again, in a transaction of its own, asking the shortlist first; false
    /// if it has no target now.
    async fn retry_placement(&self, id: Uuid) -> Result<bool, AppError> {
        let screened = self.screen_current(id).await?;
        let mut tx = self.store.begin().await?;
        let Some(w) = tx.lock_workload(id).await? else { return Ok(false) };
        let Some(t) = w.target.filter(|_| w.deleted_at.is_none()) else { return Ok(false) };
        let target = tx.intent_within(t).await?;
        self.place_screened(&mut tx, &w, target.as_ref(), &screened).await?;
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
        self.place_screened(tx, w, target, &Screened::default()).await
    }

    /// As [`Self::place_target`], with what a shortlist query found (§2.1 of
    /// docs/05-scheduling.md): its refusals kept beside every refusal recorded for the target,
    /// and the nodes that did not answer passed over.
    pub(crate) async fn place_screened(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        target: Option<&Intent>,
        screened: &Screened,
    ) -> Result<(), AppError> {
        tx.lock_placement().await?;
        self.place_locked(tx, w, target, screened).await?;
        // Where it is counted follows where it is placed now, whatever was decided (§5.1 of
        // docs/05-scheduling.md, "Where a workload runs").
        self.count_placed(tx, w).await?;
        Ok(())
    }

    /// [`Self::place_screened`], under the placement lock.
    async fn place_locked(
        &self,
        tx: &mut S::Tx,
        w: &Workload,
        target: Option<&Intent>,
        screened: &Screened,
    ) -> Result<(), AppError> {
        let current = tx.placement_in(w.id).await?;
        let Some(target) = target else {
            if current.is_some() {
                tx.unbind(w.id).await?;
                let grant = tx.decision_grant(w.id).await?;
                tx.append_effect(&place_effect(w, Outcome::RolledBack, grant)).await?;
            }
            return Ok(());
        };
        if current.as_ref().is_some_and(|p| p.intent == target.id) {
            // A cancellation put back the target it is placed for; a failure recorded for the
            // target that was withdrawn no longer says anything.
            tx.clear_unplaced(w.id).await?;
            return Ok(());
        }
        self.place_new(tx, (w, target), current, screened, None).await
    }

    /// Places `w` for `target`, which it is not placed for: on its node, where it is placed, or
    /// wherever the policy prefers; or, by `failover`, on another node reaching its data.
    pub(super) async fn place_new(
        &self,
        tx: &mut S::Tx,
        (w, target): (&Workload, &Intent),
        current: Option<Placement>,
        screened: &Screened,
        failover: Option<&Failover>,
    ) -> Result<(), AppError> {
        let owners = self.owners(w.organization).await?;
        let mut nodes = tx.serving_nodes(&owners).await?;
        if let Some(p) = &current {
            nodes.retain(|n| n.id == p.node);
        }
        let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
        let allocations = tx.allocations(&ids).await?;
        let (held, held_cores) =
            (tx.device_holds(&ids, w.id).await?, tx.core_holds(&ids, w.id).await?);
        let peers = tx.peers(w.organization, w.id).await?;
        let empty = Bounds::default();
        let candidates = candidates_of(&nodes, &allocations, &empty);
        let reached = self.reached_volumes(tx, (w, target), current.is_some());
        let (volumes, remade) = Box::pin(reached).await?;
        let refused = self.refusals_screened(tx, (w, target), screened).await?;
        let learned = self.learned(tx, &ids, target).await?;
        let (entries, placed) = self.placed_entries(tx, w).await?;
        let promised = self.placed_promises(tx, w, (&entries, placed.as_ref())).await?;
        let quota = super::placed_quota::counting(&entries, placed.as_ref(), promised);
        let addressing = self.addressing(tx, (w.id, &target.body), &owners, &nodes).await?;
        let req = Request {
            workload: w.id,
            project: w.project,
            labels: w.labels.as_map(),
            owners: &owners,
            body: &target.body,
            current: current.as_ref().map(current_of),
            edge_tier: addressing.stable(),
            routed: addressing.routed,
            ingress_edge: addressing.ingress.is_some(),
            edge_members: addressing.edge_members(),
            volumes: &volumes,
            refused: &refused,
            content: &learned.content,
            churn: &learned.churn,
            dependencies: &self.needed(w, &target.body).await?,
            mesh: &self.judged().await?.pairs(),
            quota: &quota,
            devices: &held,
            cores: &held_cores,
            dedicated: &addressing.dedicated,
        };
        let explanation = place(&req, &candidates, &peers, &self.policies(&owners).await?);
        let Some(node) = explanation.chosen else {
            return self.record_unplaced(tx, w, target, explanation).await;
        };
        let given = given_on(&req, &nodes, node)?;
        if let Some(n) = nodes.iter().find(|n| n.id == node) {
            let at = (owners.as_slice(), n);
            self.hold_addressing(tx, (w.id, &target.body), at, &addressing).await?;
        }
        let binding = Binding { w, target, current, node, given, failover, remade };
        self.bind_chosen(tx, binding, explanation).await
    }

    /// Each volume a placement of `w` for `target` must reach, with where it is: those `target`
    /// attaches; or, for a workload not `placed` that was made on a node and has nothing under
    /// way, every volume it holds, which a remake plan attaches where it lands, returned too.
    async fn reached_volumes(
        &self,
        tx: &mut S::Tx,
        (w, target): (&Workload, &Intent),
        placed: bool,
    ) -> Result<(Vec<(Uuid, Home)>, Option<Vec<Held>>), AppError> {
        if placed || !tx.made_and_settled(w.id).await? {
            return Ok((self.existing_nodes(&target.body).await?, None));
        }
        let held = self.held_volumes(w, &target.body).await?;
        Ok((held.iter().map(|h| (h.id, h.home.clone())).collect(), Some(held)))
    }

    /// Every driver's refusal of `target`: those the shortlist just found, recorded, with those
    /// recorded before, and the nodes that did not answer it.
    async fn refusals_screened(
        &self,
        tx: &mut S::Tx,
        (w, target): (&Workload, &Intent),
        screened: &Screened,
    ) -> Result<std::collections::BTreeMap<Uuid, String>, AppError> {
        let now = self.clock.now();
        let (found, unanswered) = screened.for_target(target.id);
        for (node, why) in found {
            tx.refuse((w.id, target.id), *node, why, now).await?;
        }
        let mut refused = tx.refusals(w.id, target.id).await?;
        refused.extend(unanswered.iter().map(|(n, why)| (*n, why.clone())));
        Ok(refused)
    }

    async fn bind_chosen(
        &self,
        tx: &mut S::Tx,
        binding: Binding<'_>,
        explanation: Explanation,
    ) -> Result<(), AppError> {
        let Binding { w, target, current, node, given, failover, remade } = binding;
        let followed = current.is_some();
        let fresh = if failover.is_some() { BoundBy::Failover } else { BoundBy::Scheduler };
        let (bound_at, bound_by) =
            current.map_or((self.clock.now(), fresh), |p| (p.bound_at, p.bound_by));
        let placement = Placement {
            workload: w.id,
            node,
            intent: target.id,
            reserved: given.reserved,
            bound_at,
            bound_by,
            reason: reason(followed, failover.is_some()),
            explanation,
            authoritative: true,
            held_at: None,
            devices: given.devices,
            cores: given.cores,
        };
        if failover.is_some() {
            // The placement it leaves, on the node that is gone, goes with its reservations.
            tx.unbind(w.id).await?;
        }
        tx.bind(&placement).await?;
        tx.clear_unplaced(w.id).await?;
        let grant = tx.decision_grant(w.id).await?;
        let remake = match &remade {
            // Boxed: every placement's future would otherwise carry this one's.
            Some(held) => Some(Box::pin(self.remake(tx, (w, target), held, grant)).await?),
            None => None,
        };
        let mut e = match failover {
            Some(f) => f.effect(w, &placement, (remade.as_deref().unwrap_or_default(), grant)),
            None => place_effect(w, Outcome::Succeeded, grant).after(details([
                ("node", node.to_string().into()),
                ("intent", target.id.to_string().into()),
                ("followed", followed.into()),
                ("reason", placement.reason.clone().into()),
            ])),
        };
        (e.node, e.plan) = (Some(node), remake);
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
            let grant = tx.decision_grant(w.id).await?;
            let e = place_effect(w, Outcome::Failed, grant).error(details([
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
