// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints and network policy.

use crate::scope::{selector_from_pb, selector_to_pb};
use crate::{Result, fits, invalid};
use korpis_core::workload::spec::EgressPolicy;
use korpis_core::workload::spec::endpoints::{
    ConntrackLimits, EndpointSpec, Exposure, NetworkAction, NetworkPolicy, NetworkRule, Peer,
    PortRange, Protocol, ShapingSpec, TlsSpec,
};

pub(crate) use super::tls::{client_address_from_pb, tls_from_pb, tls_to_pb};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::network_rule::Peer as P;

enum_map!(protocol_to_pb, protocol_from_pb, Protocol, pb::EndpointProtocol, "an endpoint protocol", {
    Protocol::Tcp => pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP,
    Protocol::Udp => pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP,
    Protocol::Http => pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP,
    Protocol::Https => pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTPS,
    Protocol::Quic => pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC,
});

enum_map!(exposure_to_pb, exposure_from_pb, Exposure, pb::Exposure, "an exposure", {
    Exposure::NodePort => pb::Exposure::EXPOSURE_NODE_PORT,
    Exposure::NodeBound => pb::Exposure::EXPOSURE_NODE_BOUND,
    Exposure::Stable => pb::Exposure::EXPOSURE_STABLE,
    Exposure::Ingress => pb::Exposure::EXPOSURE_INGRESS,
    Exposure::DedicatedIp => pb::Exposure::EXPOSURE_DEDICATED_IP,
    Exposure::OverlayOnly => pb::Exposure::EXPOSURE_OVERLAY_ONLY,
    Exposure::None => pb::Exposure::EXPOSURE_NONE,
});

enum_map!(action_to_pb, action_from_pb, NetworkAction, pb::NetworkAction, "allow or deny", {
    NetworkAction::Allow => pb::NetworkAction::NETWORK_ACTION_ALLOW,
    NetworkAction::Deny => pb::NetworkAction::NETWORK_ACTION_DENY,
});

fn rule_to_pb(r: &NetworkRule) -> pb::NetworkRule {
    pb::NetworkRule {
        action: action_to_pb(r.action).into(),
        peer: Some(match &r.peer {
            Peer::Cidr(c) => P::Cidr(c.to_string()),
            Peer::Workloads(s) => P::Workloads(Box::new(selector_to_pb(s))),
            Peer::Domain(d) => P::Domain(d.clone()),
        }),
        ports: r
            .ports
            .iter()
            .map(|p| pb::PortRange {
                first: u32::from(p.first),
                last: u32::from(p.last),
                ..Default::default()
            })
            .collect(),
        protocol: r
            .protocol
            .map_or(pb::EndpointProtocol::ENDPOINT_PROTOCOL_UNSPECIFIED, protocol_to_pb)
            .into(),
        ..Default::default()
    }
}

fn rule_from_pb(field: &str, r: &pb::NetworkRule) -> Result<NetworkRule> {
    let peer_field = format!("{field}.peer");
    let peer = match r.peer.as_ref().ok_or_else(|| invalid(&peer_field, "required"))? {
        P::Cidr(c) => Peer::Cidr(c.parse().map_err(|_| invalid(&peer_field, "not a CIDR"))?),
        P::Workloads(s) => Peer::Workloads(selector_from_pb(&peer_field, Some(s))?),
        P::Domain(d) => Peer::Domain(d.clone()),
    };
    let ports = r
        .ports
        .iter()
        .map(|p| {
            let f = format!("{field}.ports");
            Ok(PortRange {
                first: fits(&f, u64::from(p.first))?,
                last: fits(&f, u64::from(p.last))?,
            })
        })
        .collect::<Result<_>>()?;
    let protocol = match r.protocol.as_known() {
        Some(pb::EndpointProtocol::ENDPOINT_PROTOCOL_UNSPECIFIED) => None,
        _ => Some(protocol_from_pb(&format!("{field}.protocol"), r.protocol)?),
    };
    Ok(NetworkRule {
        action: action_from_pb(&format!("{field}.action"), r.action)?,
        peer,
        ports,
        protocol,
    })
}

fn rules_from_pb(field: &str, rules: &[pb::NetworkRule]) -> Result<Vec<NetworkRule>> {
    rules.iter().enumerate().map(|(i, r)| rule_from_pb(&format!("{field}[{i}]"), r)).collect()
}

fn limits_to_pb(c: ConntrackLimits) -> pb::ConntrackLimits {
    pb::ConntrackLimits {
        max_connections: c.max_connections,
        new_per_sec: c.new_per_sec,
        max_half_open: c.max_half_open,
        ..Default::default()
    }
}

fn limits_from_pb(c: Option<&pb::ConntrackLimits>) -> ConntrackLimits {
    c.map_or_else(ConntrackLimits::default, |c| ConntrackLimits {
        max_connections: c.max_connections,
        new_per_sec: c.new_per_sec,
        max_half_open: c.max_half_open,
    })
}

fn policy_to_pb(p: &NetworkPolicy) -> pb::NetworkPolicy {
    pb::NetworkPolicy {
        ingress: p.ingress.iter().map(rule_to_pb).collect(),
        egress: p.egress.iter().map(rule_to_pb).collect(),
        conntrack: limits_to_pb(p.conntrack).into(),
        ..Default::default()
    }
}

pub(crate) fn policy_from_pb(field: &str, p: Option<&pb::NetworkPolicy>) -> Result<NetworkPolicy> {
    let Some(p) = p else { return Ok(NetworkPolicy::default()) };
    Ok(NetworkPolicy {
        ingress: rules_from_pb(&format!("{field}.ingress"), &p.ingress)?,
        egress: rules_from_pb(&format!("{field}.egress"), &p.egress)?,
        conntrack: limits_from_pb(p.conntrack.as_option()),
    })
}

/// The wire form of a workload's outbound policy.
#[must_use]
pub fn egress_to_pb(e: &EgressPolicy) -> pb::EgressPolicy {
    pb::EgressPolicy {
        rules: e.rules.iter().map(rule_to_pb).collect(),
        limits: limits_to_pb(e.limits).into(),
        ..Default::default()
    }
}

/// A workload's outbound policy from the field `field`; absent is none beyond the default.
pub fn egress_from_pb(field: &str, e: Option<&pb::EgressPolicy>) -> Result<EgressPolicy> {
    let Some(e) = e else { return Ok(EgressPolicy::default()) };
    Ok(EgressPolicy {
        rules: rules_from_pb(&format!("{field}.rules"), &e.rules)?,
        limits: limits_from_pb(e.limits.as_option()),
    })
}

/// The wire form of an endpoint declaration.
#[must_use]
pub fn endpoint_to_pb(e: &EndpointSpec) -> pb::EndpointSpec {
    pb::EndpointSpec {
        name: e.name.clone(),
        protocol: protocol_to_pb(e.protocol).into(),
        container_port: u32::from(e.container_port),
        exposure: exposure_to_pb(e.exposure).into(),
        domains: e.domains.clone(),
        tls: e.tls.as_ref().map(tls_to_pb).into(),
        client_address: if matches!(e.tls, Some(TlsSpec::Passthrough { proxy_protocol: true })) {
            pb::ClientAddress::CLIENT_ADDRESS_PROXY_PROTOCOL.into()
        } else {
            pb::ClientAddress::CLIENT_ADDRESS_UNSPECIFIED.into()
        },
        policy: policy_to_pb(&e.policy).into(),
        accounting: e.accounting,
        shaping: pb::ShapingSpec {
            egress_bits_per_sec: e.shaping.egress_bits_per_sec,
            ingress_bits_per_sec: e.shaping.ingress_bits_per_sec,
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// An endpoint declaration from the field `field`.
pub fn endpoint_from_pb(field: &str, e: &pb::EndpointSpec) -> Result<EndpointSpec> {
    let f = |name: &str| format!("{field}.{name}");
    Ok(EndpointSpec {
        name: e.name.clone(),
        protocol: protocol_from_pb(&f("protocol"), e.protocol)?,
        container_port: fits(&f("container_port"), u64::from(e.container_port))?,
        exposure: exposure_from_pb(&f("exposure"), e.exposure)?,
        domains: e.domains.clone(),
        tls: client_address_from_pb(
            &f("client_address"),
            e,
            e.tls.as_option().map(|t| tls_from_pb(&f("tls"), t)).transpose()?,
        )?,
        policy: policy_from_pb(&f("policy"), e.policy.as_option())?,
        accounting: e.accounting,
        shaping: e.shaping.as_option().map_or_else(ShapingSpec::default, |s| ShapingSpec {
            egress_bits_per_sec: s.egress_bits_per_sec,
            ingress_bits_per_sec: s.ingress_bits_per_sec,
        }),
    })
}
