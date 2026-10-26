// SPDX-License-Identifier: AGPL-3.0-or-later

//! Address router calls in the stored queue (§3.7 of docs/07-networking.md): an address, the
//! node it is to be routed to or none, and that node's advertised addresses; answered by its
//! being routed so.

use korpis_core::address::Route;
use korpis_core::extension::{CallOutcome, CallRequest};
use serde_json::{Value, json};

/// An address router request to store, if `r` is one.
pub(crate) fn request_to(r: &CallRequest) -> Option<Value> {
    let CallRequest::RouteAddress(r) = r else { return None };
    let hops: Vec<String> = r.next_hops.iter().map(ToString::to_string).collect();
    Some(json!({ "route_address": {
        "address": r.address.to_string(), "node": r.node.map(|n| n.to_string()),
        "next_hops": hops } }))
}

/// A stored address router request, if `v` is one; `None` too where it is malformed.
pub(crate) fn request_from(v: &Value) -> Option<CallRequest> {
    let r = v.get("route_address")?;
    let address = r.get("address")?.as_str()?.parse().ok()?;
    let node = match r.get("node")? {
        Value::Null => None,
        n => Some(n.as_str()?.parse().ok()?),
    };
    let next_hops = r
        .get("next_hops")?
        .as_array()?
        .iter()
        .map(|h| h.as_str()?.parse().ok())
        .collect::<Option<Vec<_>>>()?;
    Some(CallRequest::RouteAddress(Route { address, node, next_hops }))
}

/// An address router outcome to store, if `o` is one.
pub(crate) fn outcome_to(o: &CallOutcome) -> Option<Value> {
    matches!(o, CallOutcome::AddressRouted).then(|| json!({ "address_routed": {} }))
}

/// A stored address router outcome, if `v` is one.
pub(crate) fn outcome_from(v: &Value) -> Option<CallOutcome> {
    v.get("address_routed").map(|_| CallOutcome::AddressRouted)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_route_is_stored_and_read_back_whole() {
        for node in [Some(uuid::Uuid::from_u128(7)), None] {
            let route = Route {
                address: "203.0.113.9".parse().unwrap(),
                node,
                next_hops: vec!["198.51.100.4".parse().unwrap(), "2001:db8::4".parse().unwrap()],
            };
            let r = CallRequest::RouteAddress(route);
            assert_eq!(request_from(&request_to(&r).unwrap()), Some(r));
        }
        let routed = CallOutcome::AddressRouted;
        assert_eq!(outcome_from(&outcome_to(&routed).unwrap()), Some(routed));
    }
}
