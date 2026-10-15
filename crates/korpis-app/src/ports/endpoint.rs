// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (§3 of docs/07-networking.md): what each endpoint of a placed workload
//! became on its node, and the ports of the node they hold.

use super::StoreResult;
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

    /// Writes `endpoint`, replacing the one of its workload with its name; created at `at` if
    /// it is new.
    fn put_endpoint(
        &mut self,
        endpoint: &Endpoint,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
