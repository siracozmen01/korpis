// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases on the addresses of edge pools, and the health of each group's members (§3.1 of
//! docs/07-networking.md).

use super::StoreResult;
use korpis_core::edge::lease::{Hold, MemberState};
use korpis_core::edge::probe::AddressStatus;
use std::collections::BTreeSet;
use std::future::Future;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Reading addresses' leases and members' health.
pub trait EdgeLeaseRead {
    /// Every address's lease.
    fn edge_holds_all(&self) -> impl Future<Output = StoreResult<Vec<Hold>>> + Send;

    /// Every member of every group, as the control plane knows it.
    fn edge_members_all(&self) -> impl Future<Output = StoreResult<Vec<MemberState>>> + Send;

    /// Each address of `group`'s pools with a lease, and its own probe.
    fn edge_addresses_of(
        &self,
        group: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<AddressStatus>>> + Send;

    /// The nodes a control plane found sharing its network, as last recorded before `now`.
    fn colocated_nodes(
        &self,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<BTreeSet<Uuid>>> + Send;
}

/// Deciding addresses' leases, under the lock of the address's group.
pub trait EdgeLeaseWrite {
    /// The group whose pool `address` is of, locked, with its term, where a `stable` endpoint
    /// holds a port of the address; none otherwise.
    fn lock_edge_group_of(
        &mut self,
        address: IpAddr,
    ) -> impl Future<Output = StoreResult<Option<(Uuid, Duration)>>> + Send;

    /// `group`'s members, as this transaction sees them.
    fn edge_members_now(
        &mut self,
        group: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<MemberState>>> + Send;

    /// `address`'s lease, locked.
    fn edge_hold_for_update(
        &mut self,
        address: IpAddr,
    ) -> impl Future<Output = StoreResult<Option<Hold>>> + Send;

    /// Records `hold`, over the address's last.
    fn put_edge_hold(&mut self, hold: &Hold) -> impl Future<Output = StoreResult<()>> + Send;

    /// Extends `address`'s lease from `at.0` for `at.1`, the term the grant then says.
    fn renew_edge_hold(
        &mut self,
        address: IpAddr,
        at: (SystemTime, Duration),
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that `replica` shares its network with each of `nodes`, until `until`, and with
    /// no other node.
    fn record_colocated(
        &mut self,
        replica: Uuid,
        nodes: &[Uuid],
        until: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Makes `replica` the prober of every group no other replica holds the probe of at
    /// `span.0`, and of those it holds, until `span.1`; the groups it probes.
    fn claim_edge_probes(
        &mut self,
        replica: Uuid,
        span: (SystemTime, SystemTime),
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Records a member's own probe at `at`: whether it passes and, where it does not, why.
    fn record_member_probe(
        &mut self,
        member: (Uuid, Uuid),
        passes: (bool, Option<&str>),
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records an address's own probe through its holder at `at`.
    fn record_address_probe(
        &mut self,
        address: IpAddr,
        answering: bool,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that a member stopped holding an address under an epoch, `held` naming the three,
    /// at `now`; whether that was its lease, unreleased.
    fn release_edge_hold(
        &mut self,
        held: (IpAddr, Uuid, u64),
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
