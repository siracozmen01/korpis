// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed: `korpis.v1.Endpoint` (§3.6 of docs/01-model.md).

use crate::spec::endpoints::{exposure_to_pb, protocol_to_pb, tls_to_pb};
use crate::wire_id;
use korpis_core::id::IdKind;
use korpis_core::workload::endpoint::Endpoint;
use korpis_core::workload::spec::endpoints::{Exposure, TlsSpec};
use korpis_proto::korpis::v1 as pb;
use std::net::IpAddr;

/// The port an ingress endpoint is reached on.
pub const INGRESS_PORT: u16 = 443;

/// The wire form of an endpoint, reached at `addresses` with its port where it holds one of its
/// node's: the node's advertised addresses, which an assignment leaves out because the node
/// forwards the port on every address it has.
#[must_use]
pub fn endpoint_to_pb(e: &Endpoint, addresses: &[IpAddr]) -> pb::Endpoint {
    let ingress = e.exposure == Exposure::Ingress;
    // An ingress endpoint is reached on 443 of its node's addresses, where its node's proxy
    // listens, and on 80 for the redirect and ACME's challenge (§3.2 of docs/07-networking.md).
    // A dedicated address is the workload's own, reached on the endpoint's own port, once it
    // holds one (§3.7 of docs/07-networking.md).
    let dedicated = e.exposure == Exposure::DedicatedIp;
    let port = match e.exposure {
        Exposure::Ingress => Some(INGRESS_PORT),
        Exposure::DedicatedIp => (!addresses.is_empty()).then_some(e.container_port),
        _ => e.port,
    };
    let address = port.map(|port| pb::Address {
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
        // docs/07-networking.md); the ingress proxy is the workload's peer, and says who the
        // client was in a header only it writes (§3.4 there), or, to a workload whose TLS it
        // passes through and that declared it, in a PROXY header (§3.2 there).
        client_address: if matches!(e.tls, Some(TlsSpec::Passthrough { proxy_protocol: true })) {
            pb::ClientAddress::CLIENT_ADDRESS_PROXY_PROTOCOL.into()
        } else if ingress {
            pb::ClientAddress::CLIENT_ADDRESS_TRANSLATED.into()
        } else if e.port.is_some() || dedicated {
            pb::ClientAddress::CLIENT_ADDRESS_PRESERVED.into()
        } else {
            pb::ClientAddress::CLIENT_ADDRESS_UNSPECIFIED.into()
        },
        domains: e.domains.clone(),
        tls: e.tls.as_ref().map(tls_to_pb).into(),
        certificate: e.certificate.map(|c| wire_id(IdKind::Certificate, c.id)).unwrap_or_default(),
        certificate_revision: e.certificate.map_or(0, |c| c.revision),
        ..Default::default()
    }
}

/// An `ingress` endpoint as placed, from the wire form [`endpoint_to_pb`] gives it: the node it
/// is placed on is its address's, and it holds no port of its node.
///
/// # Errors
/// A field that is not what it says, or an endpoint of another exposure.
pub fn ingress_endpoint_from_pb(field: &str, e: &pb::Endpoint) -> crate::Result<Endpoint> {
    use crate::spec::endpoints::{exposure_from_pb, protocol_from_pb, tls_from_pb};
    use crate::{fits, id, invalid};
    let f = |name: &str| format!("{field}.{name}");
    if exposure_from_pb(&f("exposure"), e.exposure)? != Exposure::Ingress {
        return Err(invalid(&f("exposure"), "an ingress endpoint"));
    }
    let tls = match e.tls.as_option().map(|t| tls_from_pb(&f("tls"), t)).transpose()? {
        Some(TlsSpec::Passthrough { .. }) => Some(TlsSpec::Passthrough {
            proxy_protocol: e.client_address.as_known()
                == Some(pb::ClientAddress::CLIENT_ADDRESS_PROXY_PROTOCOL),
        }),
        other => other,
    };
    let node = e.address.as_option().map_or("", |a| a.node.as_str());
    let certificate = if e.certificate.is_empty() {
        None
    } else {
        let at = id(IdKind::Certificate, &f("certificate"), &e.certificate)?;
        Some(korpis_core::workload::endpoint::Served { id: at, revision: e.certificate_revision })
    };
    Ok(Endpoint {
        id: id(IdKind::Endpoint, &f("id"), &e.id)?,
        workload: id(IdKind::Workload, &f("workload"), &e.workload)?,
        name: e.name.clone(),
        protocol: protocol_from_pb(&f("protocol"), e.protocol)?,
        container_port: fits(&f("container_port"), u64::from(e.container_port))?,
        exposure: Exposure::Ingress,
        node: id(IdKind::Node, &f("address.node"), node)?,
        port: None,
        domains: e.domains.clone(),
        tls,
        certificate,
    })
}
