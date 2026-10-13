// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agent sessions (§4.2 and §7 of docs/02-architecture.md): what a node reports about itself,
//! the nonces that keep a Hello from being replayed, and whether a node is heard.

use super::StoreResult;
use korpis_core::node::{DriverInfo, StorageClass};
use korpis_core::tenancy::quota::Bounds;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// What a node's agent reports about it, in a Hello or a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFacts {
    /// Total.
    pub capacity: Bounds,
    /// What placements may reserve.
    pub allocatable: Bounds,
    /// The runtime drivers, with their declared capabilities.
    pub drivers: Vec<DriverInfo>,
    /// The storage classes; `None` leaves them as they were, since a heartbeat does not carry
    /// them.
    pub storage_classes: Option<Vec<StorageClass>>,
    /// The agent's release and the protocol the session speaks; a Hello says, a heartbeat
    /// does not.
    pub agent: Option<AgentVersion>,
    /// What it advertises for inbound traffic; `None` leaves it as it was, for an agent that
    /// does not say.
    pub advertised: Option<korpis_core::node::advertised::Advertised>,
}

/// An agent's release and protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentVersion {
    /// The release version.
    pub release: String,
    /// The protocol version the session speaks.
    pub protocol: String,
}

/// Recording what sessions learn.
pub trait SessionWrite {
    /// Records `nonce` as seen from `node` at `at`, forgetting that node's nonces older than
    /// `forget_before`; false if it was already seen.
    fn remember_nonce(
        &mut self,
        node: Uuid,
        nonce: &[u8],
        at: SystemTime,
        forget_before: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records what the node reported at `at`, and that it is connected and was heard then.
    /// False if the node does not exist.
    fn record_node_facts(
        &mut self,
        node: Uuid,
        facts: &NodeFacts,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Marks unreachable every connected node last heard before `before`; how many.
    fn mark_silent_nodes(
        &mut self,
        before: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Marks unknown every node the store says is connected: a control plane that has just
    /// started has heard none of them; how many.
    fn forget_connections(&mut self) -> impl Future<Output = StoreResult<u64>> + Send;
}
