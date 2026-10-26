// SPDX-License-Identifier: AGPL-3.0-or-later

//! Address pools: `korpis.v1.AddressPool` (§3.6 of docs/01-model.md, §3.7 of
//! docs/07-networking.md).

use crate::{Result, duration, id, invalid, timestamp, wire_id};
use korpis_core::address::{AddressPool, Routing, Scope};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a pool, `held` of whose addresses workloads hold.
#[must_use]
pub fn pool_to_pb(p: &AddressPool, held: u64) -> pb::AddressPool {
    use pb::AddressPoolScope as S;
    let (scope, node, router) = match &p.scope {
        Scope::Node(n) => (S::ADDRESS_POOL_SCOPE_NODE, wire_id(IdKind::Node, *n), String::new()),
        Scope::Edge => (S::ADDRESS_POOL_SCOPE_EDGE, String::new(), String::new()),
        Scope::Cluster(r) => (S::ADDRESS_POOL_SCOPE_CLUSTER, String::new(), r.clone()),
    };
    pb::AddressPool {
        id: wire_id(IdKind::AddressPool, p.id),
        organization: wire_id(IdKind::Organization, p.organization),
        cidr: p.cidr.to_string(),
        scope: scope.into(),
        node,
        router,
        routing: routing_to_pb(p.routing).into(),
        interface: p.interface.clone(),
        quarantine: duration(p.quarantine).into(),
        reserved: p.reserved.iter().map(ToString::to_string).collect(),
        held,
        created_at: timestamp(p.created_at).into(),
        ..Default::default()
    }
}

/// The wire form of how a pool's addresses reach their node.
#[must_use]
pub fn routing_to_pb(r: Routing) -> pb::AddressRouting {
    match r {
        Routing::Static => pb::AddressRouting::ADDRESS_ROUTING_STATIC,
        Routing::Bgp => pb::AddressRouting::ADDRESS_ROUTING_BGP,
        Routing::ProviderApi => pb::AddressRouting::ADDRESS_ROUTING_PROVIDER_API,
    }
}

/// Where a pool asked for from the wire holds its addresses: its `scope`, with the `node` or
/// `router` it names.
///
/// # Errors
/// A scope unset, or the identifier it needs missing or malformed.
pub fn scope_from_pb(p: &pb::AddressPool) -> Result<Scope> {
    match p.scope.as_known() {
        Some(pb::AddressPoolScope::ADDRESS_POOL_SCOPE_NODE) => {
            Ok(Scope::Node(id(IdKind::Node, "pool.node", &p.node)?))
        }
        Some(pb::AddressPoolScope::ADDRESS_POOL_SCOPE_EDGE) => Ok(Scope::Edge),
        Some(pb::AddressPoolScope::ADDRESS_POOL_SCOPE_CLUSTER) => {
            Ok(Scope::Cluster(p.router.clone()))
        }
        _ => Err(invalid("pool.scope", "node, edge, or cluster")),
    }
}

/// How a pool asked for from the wire reaches its node; a node pool's static where unset.
///
/// # Errors
/// A routing that is not one.
pub fn routing_from_pb(p: &pb::AddressPool) -> Result<Routing> {
    match p.routing.as_known() {
        Some(pb::AddressRouting::ADDRESS_ROUTING_STATIC) => Ok(Routing::Static),
        Some(pb::AddressRouting::ADDRESS_ROUTING_BGP) => Ok(Routing::Bgp),
        Some(pb::AddressRouting::ADDRESS_ROUTING_PROVIDER_API) => Ok(Routing::ProviderApi),
        Some(pb::AddressRouting::ADDRESS_ROUTING_UNSPECIFIED)
            if p.scope.as_known() == Some(pb::AddressPoolScope::ADDRESS_POOL_SCOPE_NODE) =>
        {
            Ok(Routing::Static)
        }
        _ => Err(invalid("pool.routing", "static, bgp, or provider_api")),
    }
}

/// A workload's dedicated address as its status shows it: `held`, and whether it waits on its
/// router.
#[must_use]
pub fn dedicated_status_to_pb(
    (held, waiting): &(korpis_core::address::Holding, bool),
) -> pb::DedicatedAddressStatus {
    pb::DedicatedAddressStatus {
        address: held.address.to_string(),
        waiting_on_router: *waiting,
        reason: held.failing.clone().unwrap_or_default(),
        ..Default::default()
    }
}
