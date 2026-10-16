// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (§3 of docs/07-networking.md): what each endpoint of a placed workload
//! became on its node, and the ports of the node they hold.

use super::StoreResult;
use korpis_core::dns::published::EndpointRecord;
use korpis_core::node::advertised::PortRange;
use korpis_core::workload::endpoint::Endpoint;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading endpoints.
pub trait EndpointRead {
    /// `workload`'s endpoints, sorted by name.
    fn endpoints(&self, workload: Uuid) -> impl Future<Output = StoreResult<Vec<Endpoint>>> + Send;

    /// The ports of `node` its endpoints hold.
    fn node_ports_held(&self, node: Uuid) -> impl Future<Output = StoreResult<Vec<u16>>> + Send;

    /// One endpoint, by its identifier.
    fn endpoint(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Endpoint>>> + Send;

    /// The names of `workload`'s ingress endpoints in DNS, routed now (§3.5 of
    /// docs/07-networking.md).
    fn endpoint_records(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<EndpointRecord>>> + Send;
}

/// Writing endpoints, in the transaction that computes a node's assignments.
pub trait EndpointWrite {
    /// Gives back what endpoints on `node` hold for workloads that were retired, moves every
    /// endpoint of `workload` on another node to `node` without the port it held there, and
    /// removes each of its endpoints whose name `keep` lacks.
    fn release_endpoints(
        &mut self,
        node: Uuid,
        workload: Uuid,
        keep: &[String],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// `workload`'s endpoints, sorted by name, as this transaction sees them.
    fn held_endpoints(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Endpoint>>> + Send;

    /// The lowest port of `range` no endpoint on `node` holds, if any is free.
    fn free_node_port(
        &mut self,
        node: Uuid,
        range: PortRange,
    ) -> impl Future<Output = StoreResult<Option<u16>>> + Send;

    /// Makes the names in DNS follow the endpoints the ingress serves: a name routed and not yet
    /// recorded is recorded pending, one whose node's addresses changed is due again, and one no
    /// longer routed is due to be withdrawn, or forgotten if nothing was published for it.
    fn sync_endpoint_records(
        &mut self,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// A name due at `now`, held for the caller until `until`: pending, failed or unmanaged past
    /// its next attempt, or wanting other addresses than it was given.
    fn take_due_record(
        &mut self,
        now: SystemTime,
        until: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<EndpointRecord>>> + Send;

    /// Writes what publishing `record` came to, and releases it.
    fn put_endpoint_record(
        &mut self,
        record: &EndpointRecord,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Forgets a name withdrawn.
    fn drop_endpoint_record(
        &mut self,
        endpoint: Uuid,
        name: &str,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Writes `endpoint`, replacing the one of its workload with its name; created at `at` if
    /// it is new.
    fn put_endpoint(
        &mut self,
        endpoint: &Endpoint,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
