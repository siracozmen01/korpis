// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ingress` through edge groups (§3.1 of docs/07-networking.md): each group's ingress addresses,
//! the workloads whose `ingress` endpoints a group serves, and the challenges each node's proxy
//! says it holds of the names its groups serve.

use super::StoreResult;
use korpis_core::edge::IngressAddress;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// The challenges a node's proxy holds of its groups' names: each HTTP-01 token and each
/// TLS-ALPN-01 digest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChallengesHeld {
    /// The HTTP-01 tokens.
    pub tokens: Vec<String>,
    /// The TLS-ALPN-01 digests.
    pub digests: Vec<Vec<u8>>,
}

/// Reading what groups serve of the ingress.
pub trait EdgeIngressRead {
    /// Every group's ingress addresses.
    fn ingress_addresses_all(
        &self,
    ) -> impl Future<Output = StoreResult<Vec<IngressAddress>>> + Send;

    /// `group`'s ingress addresses, IPv4 first.
    fn ingress_addresses_of(
        &self,
        group: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<IngressAddress>>> + Send;

    /// Every workload whose `ingress` endpoints a group serves, with the group.
    fn ingress_served_all(&self) -> impl Future<Output = StoreResult<Vec<(Uuid, Uuid)>>> + Send;

    /// The group serving `workload`'s `ingress` endpoints, if one does.
    fn ingress_served_of(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Uuid>>> + Send;

    /// The workloads whose `ingress` endpoints a group `node` is a member of serves.
    fn proxied_by(&self, node: Uuid) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// What `node` last said its proxy holds of its groups' challenges.
    fn edge_challenges_held(
        &self,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<ChallengesHeld>> + Send;
}

/// Choosing ingress addresses and recording what groups serve, in the transaction that places a
/// workload.
pub trait EdgeIngressWrite {
    /// `group`'s ingress addresses, as this transaction sees them.
    fn ingress_addresses_now(
        &mut self,
        group: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<IngressAddress>>> + Send;

    /// Records `address` as its group's ingress address of its family.
    fn hold_ingress_address(
        &mut self,
        address: &IngressAddress,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that `group` serves `workload`'s `ingress` endpoints, or, with none, that no group
    /// does.
    fn serve_ingress(
        &mut self,
        workload: Uuid,
        group: Option<Uuid>,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records what `node`'s proxy holds of its groups' challenges.
    fn record_edge_challenges(
        &mut self,
        node: Uuid,
        held: &ChallengesHeld,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
