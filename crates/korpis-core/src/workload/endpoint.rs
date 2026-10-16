// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints as placed (§3.6 of docs/01-model.md, §3 of docs/07-networking.md): what each
//! endpoint a placed workload's target declares became on its node, with the exposure it
//! resolved to and, for one the node itself exposes, the port of the node it holds.

use super::spec::endpoints::{EndpointSpec, Exposure, Protocol, TlsSpec};
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

/// What an endpoint's address survives (§3 of docs/07-networking.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Survival {
    /// No move: `node_port`, the node's address and a port that a move gives up.
    NoMove,
    /// A move, for as long as the node it was first exposed on exists: `node_bound`.
    WhileItsNodeExists,
    /// A move, and the loss of any one node: `stable`, `ingress`, `dedicated_ip`,
    /// `overlay_only`.
    Moves,
    /// Nothing reaches it: `none`.
    Unreachable,
}

impl Exposure {
    /// What an endpoint of this resolved exposure survives.
    #[must_use]
    pub const fn survives(self) -> Survival {
        match self {
            Self::NodePort => Survival::NoMove,
            Self::NodeBound => Survival::WhileItsNodeExists,
            Self::Stable | Self::Ingress | Self::DedicatedIp | Self::OverlayOnly => Survival::Moves,
            Self::None => Survival::Unreachable,
        }
    }
}

/// What a plan does to one endpoint's exposure: the mode declared, the one it resolves to here,
/// and what that survives, which the plan that creates or changes it shows (§3 of
/// docs/07-networking.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureImpact {
    /// The endpoint's name.
    pub endpoint: String,
    /// The exposure declared.
    pub declared: Exposure,
    /// The exposure it resolves to in this installation.
    pub resolved: Exposure,
    /// What the resolved one survives.
    pub survives: Survival,
    /// Whether it is reached on a node whose reachability the control plane measured itself,
    /// which may be inside the node's network: a firewall in front of the node, such as a cloud
    /// provider's, is then the operator's to open (§9 of docs/07-networking.md).
    pub measured_inside: bool,
}

/// What a plan from `from` to `to` does to the exposures of the endpoints it creates or whose
/// declared exposure it changes, where an edge tier does or does not serve the workload.
#[must_use]
pub fn exposures(
    from: Option<&[EndpointSpec]>,
    to: &[EndpointSpec],
    edge_tier: bool,
) -> Vec<ExposureImpact> {
    let unchanged = |e: &EndpointSpec| {
        from.is_some_and(|f| f.iter().any(|b| b.name == e.name && b.exposure == e.exposure))
    };
    to.iter()
        .filter(|e| !unchanged(e))
        .map(|e| {
            let resolved = e.exposure.resolved(edge_tier);
            ExposureImpact {
                endpoint: e.name.clone(),
                declared: e.exposure,
                resolved,
                survives: resolved.survives(),
                measured_inside: resolved.holds_node_port(),
            }
        })
        .collect()
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
    /// For `ingress`: the names it routes (§3.2 of docs/07-networking.md).
    pub domains: Vec<String>,
    /// For `ingress`: where its certificate comes from, `acme` when the declaration left it to
    /// the default; or that TLS is passed through to the workload.
    pub tls: Option<TlsSpec>,
    /// For `ingress` terminated by the proxy: the certificate it is served with, once one with a
    /// key is held, and its revision, so its node fetches it again when it changes.
    pub certificate: Option<Served>,
}

/// A certificate an endpoint is served with, at a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Served {
    /// The certificate.
    pub id: Uuid,
    /// Its revision.
    pub revision: i64,
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
    fn a_plan_shows_what_each_new_or_changed_exposure_resolves_to_and_survives() {
        let before = [declared(Exposure::NodePort, Protocol::Udp)];
        let mut after = vec![declared(Exposure::Stable, Protocol::Tcp)];
        let shown = exposures(Some(&before), &after, false);
        assert_eq!(shown.len(), 1, "a changed exposure is shown");
        assert_eq!((shown[0].declared, shown[0].resolved), (Exposure::Stable, Exposure::NodeBound));
        assert_eq!(shown[0].survives, Survival::WhileItsNodeExists);
        assert!(shown[0].measured_inside);
        assert!(exposures(Some(&after), &after, false).is_empty(), "unchanged is not shown");
        after[0].exposure = Exposure::OverlayOnly;
        let shown = exposures(None, &after, false);
        assert_eq!((shown[0].survives, shown[0].measured_inside), (Survival::Moves, false));
    }

    #[test]
    fn http_and_https_ride_tcp_and_quic_rides_udp() {
        assert_eq!(Protocol::Http.transport(), Transport::Tcp);
        assert_eq!(Protocol::Https.transport(), Transport::Tcp);
        assert_eq!(Protocol::Quic.transport(), Transport::Udp);
    }
}
