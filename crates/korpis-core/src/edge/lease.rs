// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases on a group's addresses (§3.1 of docs/07-networking.md), fenced as a workload's are
//! (§4.5 of docs/02-architecture.md): the member asks and renews, its lease runs from its request
//! and the grantor's from its receipt, and an address is issued to another member only once its
//! holder has released it or the grantor's end of its term has passed. Which member should hold
//! an address is decided here too, from what the control plane knows of each member's health:
//! the holder keeps it while it is ready, and otherwise it goes to a ready member, each address
//! to its own so a group's addresses spread over its members.

use super::EdgeGroup;
use super::probe::AddressStatus;
use crate::workload::lease::{Decision, Refusal};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// What the control plane knows of a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Connected, and its own address passes the probe through its forwarding path.
    Ready,
    /// Connected, and missing the probe.
    Failing,
    /// Connected, and not yet probed.
    Unknown,
    /// Its session is down, as the control plane knows from its heartbeats.
    Disconnected,
}

/// A member as the control plane knows it: whether its session is up, and its own address's
/// probe as last recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberState {
    /// The group.
    pub group: Uuid,
    /// The member.
    pub node: Uuid,
    /// Whether its node is connected.
    pub connected: bool,
    /// Whether its own address passed the probe; none until it is probed.
    pub ready: Option<bool>,
    /// When the probe was last recorded.
    pub probed_at: Option<SystemTime>,
    /// Why it is not ready, as the probe found.
    pub reason: Option<String>,
}

impl MemberState {
    /// Its health: disconnected while its session is down, whatever the probe last said.
    #[must_use]
    pub fn health(&self) -> Health {
        match (self.connected, self.ready) {
            (false, _) => Health::Disconnected,
            (true, Some(false)) => Health::Failing,
            (true, Some(true)) => Health::Ready,
            (true, None) => Health::Unknown,
        }
    }
}

/// An address's lease as the control plane holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hold {
    /// The address.
    pub address: IpAddr,
    /// The group whose pool it is of.
    pub group: Uuid,
    /// The member it was last issued to.
    pub node: Uuid,
    /// Monotonic, never reused, from the same sequence as workloads' epochs.
    pub epoch: u64,
    /// How long it lasts from the holder's request.
    pub term: Duration,
    /// When the grantor last heard the holder ask: its end is this plus `term`.
    pub received_at: SystemTime,
    /// When the holder said it stopped holding it.
    pub released_at: Option<SystemTime>,
}

impl Hold {
    /// Whether nobody can be holding it any more: released, or past the grantor's end.
    #[must_use]
    pub fn over(&self, now: SystemTime) -> bool {
        self.released_at.is_some() || now >= self.received_at + self.term
    }

    /// The member that may be holding it at `now`.
    #[must_use]
    pub fn holder(&self, now: SystemTime) -> Option<Uuid> {
        (!self.over(now)).then_some(self.node)
    }
}

/// FNV-1a over `bytes`: a hash every replica and every version computes alike.
fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes
        .into_iter()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// The member of `among` that `address` goes to by rendezvous: each member weighed by a hash of
/// the address and itself, the heaviest taking it, so adding or losing a member moves only the
/// addresses it takes or held.
fn rendezvous(address: IpAddr, among: impl Iterator<Item = Uuid>) -> Option<Uuid> {
    let octets: Vec<u8> = match address {
        IpAddr::V4(a) => a.octets().to_vec(),
        IpAddr::V6(a) => a.octets().to_vec(),
    };
    among.max_by_key(|m| (fnv(octets.iter().copied().chain(*m.as_bytes())), *m))
}

/// The member of `members` that should hold `address`, whose lease is `current`, at `now`. A
/// holder that is ready keeps it, so an address moves only when it must. Otherwise a ready member
/// takes it. Where no member is ready, the control plane suspects its own probe before the whole
/// group, and judges by sessions alone: a holder still connected keeps it, and only one whose
/// session is down, or none, gives way to a connected member.
#[must_use]
pub fn designate(
    address: IpAddr,
    members: &[(Uuid, Health)],
    current: Option<&Hold>,
    now: SystemTime,
) -> Option<Uuid> {
    let health = |m: Uuid| members.iter().find(|(n, _)| *n == m).map(|(_, h)| *h);
    let holder = current.and_then(|h| h.holder(now)).filter(|m| health(*m).is_some());
    if let Some(m) = holder.filter(|m| health(*m) == Some(Health::Ready)) {
        return Some(m);
    }
    let with = |wanted: Health| members.iter().filter(move |(_, h)| *h == wanted).map(|(m, _)| *m);
    if let Some(m) = rendezvous(address, with(Health::Ready)) {
        return Some(m);
    }
    let connected = |m: &Uuid| matches!(health(*m), Some(Health::Unknown | Health::Failing));
    if let Some(m) = holder.filter(connected) {
        return Some(m);
    }
    // Its holder's session is down, or nobody holds it: a connected member, else nothing better
    // than the holder it has.
    let reachable = members.iter().map(|(m, _)| *m).filter(connected);
    rendezvous(address, reachable).or(holder)
}

/// The member that should hold each of `addresses`, each with its group: from the group's
/// members' `states` and each address's lease in `holds`, at `now`. An address of a group that
/// serves nothing, or that no member should hold, is absent.
#[must_use]
pub fn assigned(
    groups: &[EdgeGroup],
    addresses: &[(Uuid, IpAddr)],
    (states, holds): (&[MemberState], &[Hold]),
    now: SystemTime,
) -> BTreeMap<IpAddr, Uuid> {
    let mut out = BTreeMap::new();
    for g in groups.iter().filter(|g| g.serves()) {
        let members: Vec<(Uuid, Health)> = g
            .members
            .iter()
            .map(|m| {
                let state = states.iter().find(|s| s.group == g.id && s.node == *m);
                (*m, state.map_or(Health::Unknown, MemberState::health))
            })
            .collect();
        for (_, address) in addresses.iter().filter(|(group, _)| *group == g.id) {
            let current = holds.iter().find(|h| h.address == *address);
            if let Some(m) = designate(*address, &members, current, now) {
                out.insert(*address, m);
            }
        }
    }
    out
}

/// Why a `stable` endpoint at `address` of a group whose members are `states`, whose addresses
/// are `addresses`, is not known to be reached at `now`: none where a member holds the address,
/// its pool's router where it has one has said it routes the address to that member, and a member
/// passes the control plane's probe (Finding 190 of docs/23-walkthroughs.md). The workload keeps
/// its state and placement either way, since moving it mends nothing.
#[must_use]
pub fn unreached(
    (states, addresses): (&[MemberState], &[AddressStatus]),
    address: IpAddr,
    now: SystemTime,
) -> Option<&'static str> {
    let shown = addresses.iter().find(|a| a.hold.address == address);
    let Some(holder) = shown.and_then(|a| a.hold.holder(now)) else {
        return Some("no member of its edge group holds its address");
    };
    if let Some(route) = shown.and_then(|a| a.route.as_ref())
        && route.routed != Some(holder)
    {
        return Some("its edge pool's router has not routed its address to the member holding it");
    }
    if !states.iter().any(|s| s.health() == Health::Ready) {
        return Some("no member of its edge group passes the control plane's probe");
    }
    None
}

/// Decides a request from `node` for an address, which `designated` should hold, whose current
/// lease is `current`, at `now`. The asking node's own unreleased lease is renewed under its
/// epoch while it is still the one that should hold it, as a workload's is; a new epoch is issued
/// only once any other holder's lease is over.
#[must_use]
pub fn decide(
    node: Uuid,
    designated: Option<Uuid>,
    current: Option<&Hold>,
    now: SystemTime,
) -> Decision {
    let ours = designated == Some(node);
    if let Some(h) = current
        && h.node == node
        && h.released_at.is_none()
        && ours
    {
        return Decision::Renew { epoch: h.epoch };
    }
    let held_elsewhere = current.filter(|h| h.node != node && !h.over(now));
    match (ours, held_elsewhere) {
        (true, None) => Decision::Issue,
        (true, Some(_)) => Decision::Refuse(Refusal::Unavailable),
        (false, Some(h)) => Decision::Refuse(Refusal::Superseded { by: h.node }),
        (false, None) => Decision::Refuse(Refusal::NotAssigned),
    }
}

#[cfg(test)]
mod tests;
