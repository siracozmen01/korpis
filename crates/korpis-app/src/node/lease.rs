// SPDX-License-Identifier: AGPL-3.0-or-later

//! Granting and releasing leases (§4.5 of docs/02-architecture.md). The decision is
//! [`decide`]'s; this applies it in one transaction holding the workload, so two requests for one
//! workload are decided one after the other against what the first left.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite, EpochIssue, LeaseWrite, PlacementWrite, RecipeSource, Store, Tx,
    WorkloadWrite,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::lease::{Decision, Lease, Refusal, decide};
use korpis_core::workload::spec::Continuity;
use std::time::Duration;
use uuid::Uuid;

/// The answer to a lease request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAnswer {
    /// The node may act under `epoch` for `term` from when it asked, less the clock-rate bound.
    Granted {
        /// The epoch.
        epoch: u64,
        /// The term.
        term: Duration,
        /// The declared bound on clock-rate error, in parts per million.
        rho_ppm: u32,
        /// What the node does when the lease ends without renewal; resolved.
        on_expiry: Continuity,
    },
    /// Refused, and why.
    Refused(Refusal),
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Decides `node`'s request for `workload`'s lease.
    pub async fn request_lease(&self, node: Uuid, workload: Uuid) -> Result<LeaseAnswer, AppError> {
        let terms = self.session_terms;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Some(w) = tx.lock_workload(workload).await? else {
            return Ok(LeaseAnswer::Refused(Refusal::NotAssigned));
        };
        let current = tx.lease_for_update(workload).await?;
        let placed = tx.placement_in(workload).await?.map(|p| p.node);
        let epoch = match decide(node, placed, current.as_ref(), now) {
            Decision::Refuse(r) => return Ok(LeaseAnswer::Refused(r)),
            Decision::Renew { epoch } => {
                tx.renew_lease(workload, now).await?;
                epoch
            }
            Decision::Issue => {
                let epoch = match tx.next_epoch(now).await? {
                    EpochIssue::Issued(epoch) => epoch,
                    // Nothing is issued while a restore settles (§8 of docs/03-state.md), nor
                    // beyond the issue rate; the node asks again, as for any transient refusal.
                    EpochIssue::Settling | EpochIssue::RateSpent => {
                        return Ok(LeaseAnswer::Refused(Refusal::Unavailable));
                    }
                };
                let lease = Lease {
                    workload,
                    node,
                    epoch,
                    term: terms.lease_term,
                    received_at: now,
                    released_at: None,
                };
                tx.put_lease(&lease).await?;
                let grant = tx.decision_grant(lease.workload).await?;
                tx.append_effect(&issued(&lease, current.as_ref(), grant)).await?;
                epoch
            }
        };
        tx.commit().await?;
        let continuity = match w.target {
            Some(t) => self.store.intent(t).await?.map(|i| i.body.continuity),
            None => None,
        };
        Ok(LeaseAnswer::Granted {
            epoch,
            term: terms.lease_term,
            rho_ppm: terms.rho_ppm,
            // As the assignment resolves it (§4.8 of docs/02-architecture.md).
            on_expiry: continuity.unwrap_or_default().resolved(false),
        })
    }

    /// Records that `node` stopped acting for `workload` under `epoch`; false if that was not
    /// the lease, or it was already released.
    pub async fn release_lease(
        &self,
        node: Uuid,
        workload: Uuid,
        epoch: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let released = tx.release_lease(workload, node, epoch, self.clock.now()).await?;
        tx.commit().await?;
        Ok(released)
    }
}

/// The effect of issuing an epoch: a decision of the control plane's own, naming the holder it
/// replaced and the grant of the decision the holder carries out. Renewals are not effects: they
/// change nothing anyone is told.
fn issued(lease: &Lease, previous: Option<&Lease>, grant: Option<Uuid>) -> Effect {
    let actor = Principal::System(SystemActor::Reconciler);
    let mut e = Effect::new(actor, "lease.issue", Outcome::Succeeded).after(details([
        ("epoch", lease.epoch.into()),
        ("node", lease.node.to_string().into()),
        ("previous_node", previous.map(|p| p.node.to_string()).into()),
        ("previous_epoch", previous.map(|p| p.epoch).into()),
    ]));
    e.workload = Some(lease.workload);
    e.node = Some(lease.node);
    e.grant = grant;
    e
}
