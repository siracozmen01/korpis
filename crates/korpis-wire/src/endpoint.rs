// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed: `korpis.v1.Endpoint` (§3.6 of docs/01-model.md).

use crate::spec::endpoints::{exposure_to_pb, protocol_to_pb};
use crate::wire_id;
use korpis_core::id::IdKind;
use korpis_core::workload::endpoint::Endpoint;
use korpis_proto::korpis::v1 as pb;
use std::net::IpAddr;

/// The wire form of an endpoint, reached at `addresses` with its port where it holds one of its
/// node's: the node's advertised addresses, which an assignment leaves out because the node
/// forwards the port on every address it has.
#[must_use]
pub fn endpoint_to_pb(e: &Endpoint, addresses: &[IpAddr]) -> pb::Endpoint {
    let address = e.port.map(|port| pb::Address {
        ips: addresses.iter().map(ToString::to_string).collect(),
        port: u32::from(port),
        node: wire_id(IdKind::Node, e.node),
        ..Default::default()
    });
    pb::Endpoint {
        id: wire_id(IdKind::Endpoint, e.id),
        workload: wire_id(IdKind::Workload, e.workload),
        name: e.name.clone(),
        protocol: protocol_to_pb(e.protocol).into(),
        container_port: u32::from(e.container_port),
        exposure: exposure_to_pb(e.exposure).into(),
        address: address.into(),
        // The node forwards to the workload by translating the destination alone (§3 of
        // docs/07-networking.md).
        client_address: if e.port.is_some() {
            pb::ClientAddress::CLIENT_ADDRESS_PRESERVED.into()
        } else {
            pb::ClientAddress::CLIENT_ADDRESS_UNSPECIFIED.into()
        },
        ..Default::default()
    }
}
