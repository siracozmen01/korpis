// SPDX-License-Identifier: AGPL-3.0-or-later

//! The member's leases on the edge addresses it is told to hold (§3.1 of docs/07-networking.md),
//! kept as a workload's are (§4.5 of docs/02-architecture.md) in a book of their own: an address
//! is asked for while the control plane's table names it for this member, renewed a third of the
//! way through its term, and held only while its lease is valid on this machine's own clock,
//! counted from the request. An address the helper held and holds no longer is released once
//! the helper has taken it off and forgotten its connections, so the next member is issued it
//! without waiting out the term.

use crate::lease::{Book, Grant, OnExpiry, Refusal, Request, Standing, nonce};
use crate::session::Event;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use std::collections::BTreeMap;
use tokio::time::Instant;

/// An address held: its interface, empty where it is routed here, and the epoch it is held
/// under.
pub(super) type Holding = BTreeMap<String, (String, u64)>;

/// Each address `told` names for this member to hold, with its interface.
fn wanted(told: &agent::Edges) -> BTreeMap<String, String> {
    told.groups
        .iter()
        .flat_map(|g| &g.hold)
        .map(|h| (h.address.clone(), h.interface.clone()))
        .collect()
}

fn up(message: Up) -> agent::AgentMessage {
    agent::AgentMessage { message: Some(message), ..Default::default() }
}

fn request(r: Request) -> agent::AgentMessage {
    up(Up::EdgeLeaseRequest(Box::new(agent::EdgeLeaseRequest {
        address: r.workload,
        nonce: r.nonce,
        held_epoch: r.held_epoch,
        ..Default::default()
    })))
}

fn refusal(r: &agent::EdgeLeaseRefusal) -> Refusal {
    match r.reason.as_known() {
        Some(agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_SUPERSEDED) => {
            Refusal::Superseded { by: r.superseded_by.clone() }
        }
        Some(agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_NOT_ASSIGNED) => Refusal::NotAssigned,
        _ => Refusal::Unavailable,
    }
}

/// The member's edge leases, and what the helper held at the last keep.
#[derive(Debug, Default)]
pub(super) struct EdgeLeases {
    book: Book,
    held: Holding,
}

impl EdgeLeases {
    /// The requests due at `now` for the addresses `told` names, forgetting the rest.
    pub(super) fn due(&mut self, told: &agent::Edges, now: Instant) -> Vec<agent::AgentMessage> {
        let wanted = wanted(told).into_keys().map(|a| (a, OnExpiry::Stop)).collect();
        self.book.due(&wanted, now.into_std(), nonce).into_iter().map(request).collect()
    }

    /// Takes `e` if it answers a request; whether what is held may have changed.
    pub(super) fn answer(&mut self, e: &Event, now: Instant) -> bool {
        match e {
            Event::EdgeLeaseGrant(g) => {
                let term =
                    g.term.as_option().and_then(|t| korpis_wire::duration_from("term", t).ok());
                let Some(term) = term else {
                    tracing::warn!(address = %g.address, "an edge lease grant without a term");
                    return false;
                };
                let grant =
                    Grant { epoch: g.epoch, term, rho_ppm: g.rho_ppm, on_expiry: OnExpiry::Stop };
                self.book.granted(&g.address, &g.nonce, &grant)
            }
            Event::EdgeLeaseRefusal(r) => {
                tracing::info!(address = %r.address, reason = ?refusal(r), "edge lease refused");
                self.book.refused(&r.address, &r.nonce, refusal(r), now.into_std())
            }
            _ => false,
        }
    }

    /// What the member holds at `now`: each address `told` names whose lease is valid.
    pub(super) fn holding(&self, told: &agent::Edges, now: Instant) -> Holding {
        wanted(told)
            .into_iter()
            .filter_map(|(address, interface)| match self.book.standing(&address, now.into_std()) {
                Standing::Held { epoch } => Some((address, (interface, epoch))),
                _ => None,
            })
            .collect()
    }

    /// Whether what the member holds at `now` differs from what the helper held at the last keep.
    pub(super) fn changed(&self, told: &agent::Edges, now: Instant) -> bool {
        self.holding(told, now) != self.held
    }

    /// When something next changes: a renewal falling due, or a lease running out.
    pub(super) fn next_change(&self) -> Option<Instant> {
        self.book.next_change().map(Instant::from_std)
    }

    /// Records that the helper holds exactly `holding` now; the releases of what it held before,
    /// under an epoch it no longer holds the address under.
    pub(super) fn kept(&mut self, holding: Holding) -> Vec<agent::AgentMessage> {
        let released = self
            .held
            .iter()
            .filter(|(address, (_, epoch))| holding.get(*address).map(|(_, e)| e) != Some(epoch))
            .map(|(address, (_, epoch))| {
                up(Up::EdgeLeaseReleased(Box::new(agent::EdgeLeaseReleased {
                    address: address.clone(),
                    epoch: *epoch,
                    ..Default::default()
                })))
            })
            .collect();
        self.held = holding;
        released
    }
}

#[cfg(test)]
mod tests;
