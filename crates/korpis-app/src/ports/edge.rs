// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edge groups and the stable addresses and ports of `stable` endpoints (§3.1 of
//! docs/07-networking.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::edge::{EdgeGroup, StablePort, TakenPorts};
use std::future::Future;
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading edge groups and what `stable` endpoints hold.
pub trait EdgeRead {
    /// One group, with its members.
    fn edge_group(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<EdgeGroup>>> + Send;

    /// A page of an organization's groups, in identifier order.
    fn edge_groups_of(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<EdgeGroup>>> + Send;

    /// Every group of the installation, with its members.
    fn edge_groups_all(&self) -> impl Future<Output = StoreResult<Vec<EdgeGroup>>> + Send;

    /// Whether an address pool names `group`.
    fn edge_group_named(&self, group: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// What `workload`'s `stable` endpoints hold.
    fn stable_of(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<StablePort>>> + Send;

    /// Every address and port `stable` endpoints hold, with the group whose pool it is of.
    fn stable_all(&self) -> impl Future<Output = StoreResult<Vec<(Uuid, StablePort)>>> + Send;
}

/// Writing edge groups and giving their addresses and ports, which placement does under its
/// lock.
pub trait EdgeWrite {
    /// Records a group and its members. One whose name its organization has is
    /// [`Unique::EdgeGroupName`](super::Unique::EdgeGroupName).
    fn insert_edge_group(&mut self, g: &EdgeGroup) -> impl Future<Output = StoreResult<()>> + Send;

    /// Replaces a group's members and settings; whether there was one.
    fn update_edge_group(
        &mut self,
        g: &EdgeGroup,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Deletes a group no pool names; whether there was one.
    fn delete_edge_group(&mut self, id: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// The groups of `organizations`, with their members, as this transaction sees them.
    fn edge_groups_now(
        &mut self,
        organizations: &[Uuid],
    ) -> impl Future<Output = StoreResult<Vec<EdgeGroup>>> + Send;

    /// What holds each address and port of `pool` that has a row, as this transaction sees it.
    fn stable_taken(&mut self, pool: Uuid) -> impl Future<Output = StoreResult<TakenPorts>> + Send;

    /// What `workload`'s `stable` endpoints hold, as this transaction sees it.
    fn stable_now(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<StablePort>>> + Send;

    /// Gives `held` to its workload's endpoint, over a row of it whose rest has ended.
    fn hold_stable(
        &mut self,
        held: &StablePort,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Takes back what `workload`'s endpoints hold, but those named in `keep`, to rest from
    /// `at`; how many it took back.
    fn release_stable(
        &mut self,
        workload: Uuid,
        keep: &[String],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Takes back one address, port, and transport, to rest from `when`.
    fn release_stable_port(
        &mut self,
        at: (IpAddr, u16, korpis_core::workload::endpoint::Transport),
        when: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Whether an address of `pool` is held by an endpoint, for deleting it.
    fn stable_held_in(
        &mut self,
        pool: Uuid,
    ) -> impl Future<Output = StoreResult<Option<IpAddr>>> + Send;
}
