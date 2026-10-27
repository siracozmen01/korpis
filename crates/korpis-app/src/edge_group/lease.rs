// SPDX-License-Identifier: AGPL-3.0-or-later

//! Granting and releasing leases on the addresses of edge pools (§3.1 of
//! docs/07-networking.md). The decision is [`decide`]'s, for the member [`designate`] names from
//! the group's health; this applies it in one transaction holding the address's group, so two
//! members asking for one address are decided one after the other, against what the first left.
//! Epochs come from the sequence workloads' do, spent from the asking node's own bucket first.

use crate::App;
use crate::error::AppError;
use crate::node::node_issue;
use crate::ports::{
    Clock, EdgeIngressWrite as _, EdgeLeaseWrite, EffectWrite, EpochIssue, RecipeSource, Store, Tx,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::edge::lease::{Hold, decide, designate};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::lease::{Decision, Refusal};
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// The answer to a member's request for an address's lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeLeaseAnswer {
    /// The member may hold the address under `epoch` for `term` from when it asked, less the
    /// clock-rate bound.
    Granted {
        /// The epoch.
        epoch: u64,
        /// The term.
        term: Duration,
        /// The declared bound on clock-rate error, in parts per million.
        rho_ppm: u32,
    },
    /// Refused, and why.
    Refused(Refusal),
}

/// The effect of issuing an address's epoch: a decision of the control plane's own, naming the
/// member it replaced. Renewals are not effects.
fn issued(h: &Hold, previous: Option<&Hold>) -> Effect {
    let actor = Principal::System(SystemActor::Reconciler);
    let mut e = Effect::new(actor, "edge_lease.issue", Outcome::Succeeded).after(details([
        ("address", h.address.to_string().into()),
        ("group", h.group.to_string().into()),
        ("epoch", h.epoch.into()),
        ("node", h.node.to_string().into()),
        ("previous_node", previous.map(|p| p.node.to_string()).into()),
        ("previous_epoch", previous.map(|p| p.epoch).into()),
    ]));
    e.node = Some(h.node);
    e.object = Some(ObjectRef::new(Kind::Node, h.node));
    e
}

/// A new epoch for `node`, spent from its bucket and the installation's; none where nothing may
/// be issued now, which the member treats as a transient refusal.
async fn epoch_for<T: Tx>(
    tx: &mut T,
    node: Uuid,
    now: SystemTime,
) -> Result<Option<u64>, AppError> {
    let charged = tx.charge_within(vec![node_issue(node)], now).await?;
    if charged.iter().any(|s| !s.admitted) {
        return Ok(None);
    }
    Ok(match tx.next_epoch(now).await? {
        EpochIssue::Issued(epoch) => Some(epoch),
        EpochIssue::Settling | EpochIssue::RateSpent => None,
    })
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Decides member `node`'s request for `address`'s lease.
    ///
    /// # Errors
    /// The store failed.
    pub async fn request_edge_lease(
        &self,
        node: Uuid,
        address: IpAddr,
    ) -> Result<EdgeLeaseAnswer, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Some((group, term)) = tx.lock_edge_group_of(address).await? else {
            return Ok(EdgeLeaseAnswer::Refused(Refusal::NotAssigned));
        };
        let mut members: Vec<_> =
            tx.edge_members_now(group).await?.iter().map(|m| (m.node, m.health())).collect();
        // A group's ingress address is held by a member serving the ingress alone.
        if tx.ingress_addresses_now(group).await?.iter().any(|i| i.address == address) {
            let nodes: Vec<Uuid> = members.iter().map(|(n, _)| *n).collect();
            let proxying = self.proxying(&nodes).await?;
            members.retain(|(n, _)| proxying.contains(n));
        }
        let current = tx.edge_hold_for_update(address).await?;
        let designated = designate(address, &members, current.as_ref(), now);
        let epoch = match decide(node, designated, current.as_ref(), now) {
            Decision::Refuse(r) => return Ok(EdgeLeaseAnswer::Refused(r)),
            Decision::Renew { epoch } => {
                tx.renew_edge_hold(address, (now, term)).await?;
                epoch
            }
            Decision::Issue => {
                let Some(epoch) = epoch_for(&mut tx, node, now).await? else {
                    return Ok(EdgeLeaseAnswer::Refused(Refusal::Unavailable));
                };
                let hold =
                    Hold { address, group, node, epoch, term, received_at: now, released_at: None };
                tx.put_edge_hold(&hold).await?;
                tx.append_effect(&issued(&hold, current.as_ref())).await?;
                epoch
            }
        };
        tx.commit().await?;
        Ok(EdgeLeaseAnswer::Granted { epoch, term, rho_ppm: self.session_terms.rho_ppm })
    }

    /// Records that member `node` stopped holding `address` under `epoch`; false if that was not
    /// its lease, or it was released already.
    ///
    /// # Errors
    /// The store failed.
    pub async fn release_edge_lease(
        &self,
        node: Uuid,
        address: IpAddr,
        epoch: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let released = tx.release_edge_hold((address, node, epoch), self.clock.now()).await?;
        tx.commit().await?;
        Ok(released)
    }
}
