// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (§3.6 of docs/01-model.md, §3 of docs/07-networking.md): what each
//! endpoint a placed workload's target declares became on its node, with the exposure it
//! resolved to and, for one the node itself exposes, the port of the node it holds.

use super::spec::endpoints::{EndpointSpec, Exposure, Protocol};
use uuid::Uuid;

/// The transport a protocol rides on, which is what a node forwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// TCP: `tcp`, `http`, and `https`.
    Tcp,
    /// UDP: `udp` and `quic`.
    Udp,
}

impl Protocol {
    /// The transport it rides on.
    #[must_use]
    pub const fn transport(self) -> Transport {
        match self {
            Self::Tcp | Self::Http | Self::Https => Transport::Tcp,
            Self::Udp | Self::Quic => Transport::Udp,
        }
    }
}

impl Exposure {
    /// Whether an endpoint of this resolved exposure holds a port of its node: `node_port` and
    /// `node_bound`, which the node itself is reached on (§3 of docs/07-networking.md).
    #[must_use]
    pub const fn holds_node_port(self) -> bool {
        matches!(self, Self::NodePort | Self::NodeBound)
    }
}

/// How many ports of a node a placement of these endpoints holds, where the installation does
/// or does not have an edge tier.
#[must_use]
pub fn node_ports(endpoints: &[EndpointSpec], edge_tier: bool) -> u64 {
    let held = endpoints.iter().filter(|e| e.exposure.resolved(edge_tier).holds_node_port());
    u64::try_from(held.count()).unwrap_or(u64::MAX)
}

/// An endpoint of a placed workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Its identity, kept while the workload declares it.
    pub id: Uuid,
    /// The workload.
    pub workload: Uuid,
    /// Its name, unique within the workload.
    pub name: String,
    /// What it speaks.
    pub protocol: Protocol,
    /// The port inside the workload.
    pub container_port: u16,
    /// The exposure the declared one resolved to on this placement.
    pub exposure: Exposure,
    /// The node it is placed on.
    pub node: Uuid,
    /// The port of the node it holds, for an exposure that holds one.
    pub port: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::spec::endpoints::{NetworkPolicy, ShapingSpec};

    fn declared(exposure: Exposure, protocol: Protocol) -> EndpointSpec {
        EndpointSpec {
            name: "e".into(),
            protocol,
            container_port: 25565,
            exposure,
            domains: Vec::new(),
            tls: None,
            policy: NetworkPolicy::default(),
            accounting: false,
            shaping: ShapingSpec::default(),
        }
    }

    #[test]
    fn a_node_port_is_held_by_what_the_node_itself_exposes() {
        let all = [
            declared(Exposure::NodePort, Protocol::Udp),
            declared(Exposure::NodeBound, Protocol::Tcp),
            declared(Exposure::Stable, Protocol::Http),
            declared(Exposure::Ingress, Protocol::Https),
            declared(Exposure::OverlayOnly, Protocol::Tcp),
            declared(Exposure::None, Protocol::Tcp),
        ];
        assert_eq!(node_ports(&all, false), 3, "stable without an edge is node_bound");
        assert_eq!(node_ports(&all, true), 2, "an edge holds a stable address");
    }

    #[test]
    fn http_and_https_ride_tcp_and_quic_rides_udp() {
        assert_eq!(Protocol::Http.transport(), Transport::Tcp);
        assert_eq!(Protocol::Https.transport(), Transport::Tcp);
        assert_eq!(Protocol::Quic.transport(), Transport::Udp);
    }
}
