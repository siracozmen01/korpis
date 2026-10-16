// SPDX-License-Identifier: AGPL-3.0-or-later

//! Endpoints and their network policy (docs/07-networking.md).

use super::{SpecError, check_name};
use crate::labels::LabelSelector;
use ipnet::IpNet;
use std::collections::BTreeSet;

/// The protocol an endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// TCP.
    Tcp,
    /// UDP.
    Udp,
    /// HTTP.
    Http,
    /// HTTPS.
    Https,
    /// QUIC.
    Quic,
}

/// How an endpoint is reached (§3 of docs/07-networking.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// A port on the node, chosen by Korpis.
    NodePort,
    /// A port on the node's address, fixed for the placement.
    NodeBound,
    /// An address that survives migration.
    Stable,
    /// Behind the HTTP ingress, by domain.
    Ingress,
    /// An address of its own.
    DedicatedIp,
    /// Reachable only on the overlay.
    OverlayOnly,
    /// Not reachable.
    None,
}

impl Exposure {
    /// What this declared exposure is where the installation does or does not have an edge tier
    /// serving the workload (§3 of docs/07-networking.md). A declared `stable` is a preference:
    /// without an edge to hold the address it resolves to `node_bound`, which needs the node to
    /// be reachable itself. Every other mode is what it says.
    #[must_use]
    pub const fn resolved(self, edge_tier: bool) -> Self {
        match self {
            Self::Stable if !edge_tier => Self::NodeBound,
            other => other,
        }
    }
}

/// Where a certificate comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsSpec {
    /// Issued by ACME.
    Acme,
    /// Provided by its organization: a certificate it uploaded.
    Provided {
        /// The certificate, by its identifier (`crt_…`).
        secret: String,
    },
    /// Not terminated: the workload handles TLS itself.
    Passthrough,
}

/// Allow or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkAction {
    /// Allow matching traffic.
    Allow,
    /// Deny matching traffic.
    Deny,
}

/// Who a rule applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// An address range.
    Cidr(IpNet),
    /// Workloads matching a selector.
    Workloads(LabelSelector),
    /// A DNS name.
    Domain(String),
}

/// An inclusive port range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    /// The first port.
    pub first: u16,
    /// The last port, at least `first`.
    pub last: u16,
}

/// One rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRule {
    /// Allow or deny.
    pub action: NetworkAction,
    /// Who it applies to.
    pub peer: Peer,
    /// Which ports; empty for all.
    pub ports: Vec<PortRange>,
    /// Which protocol; `None` for all.
    pub protocol: Option<Protocol>,
}

/// Connection-tracking limits; zero means unset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConntrackLimits {
    /// Concurrent connections.
    pub max_connections: u32,
    /// New connections per second.
    pub new_per_sec: u32,
    /// Half-open connections.
    pub max_half_open: u32,
}

/// An endpoint's network policy: who may connect to it, and how many at once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkPolicy {
    /// Who may connect, in order; the first rule that matches decides, and a connection no rule
    /// matches is let in.
    pub ingress: Vec<NetworkRule>,
    /// Always empty: kept because the schema is frozen (rule E), and refused when set, since a
    /// workload's outbound connections are its own (`super::EgressPolicy`).
    pub egress: Vec<NetworkRule>,
    /// Limits on the connections made to it.
    pub conntrack: ConntrackLimits,
}

/// Traffic shaping, in bits per second; zero means unset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShapingSpec {
    /// Outbound.
    pub egress_bits_per_sec: u64,
    /// Inbound.
    pub ingress_bits_per_sec: u64,
}

/// One endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSpec {
    /// Unique within the workload.
    pub name: String,
    /// What it speaks.
    pub protocol: Protocol,
    /// The port inside the workload.
    pub container_port: u16,
    /// How it is reached.
    pub exposure: Exposure,
    /// Domains routed to it, for HTTP and HTTPS.
    pub domains: Vec<String>,
    /// Certificates, for HTTPS.
    pub tls: Option<TlsSpec>,
    /// Who may talk to it and to whom it may talk.
    pub policy: NetworkPolicy,
    /// Whether its traffic is metered.
    pub accounting: bool,
    /// Traffic shaping.
    pub shaping: ShapingSpec,
}

/// Whether `d` is a lowercase DNS name, optionally with a leading `*.` wildcard label.
fn is_domain(d: &str) -> bool {
    let d = d.strip_prefix("*.").unwrap_or(d);
    d.len() <= 253
        && d.split('.').count() >= 2
        && d.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
}

pub(super) fn check_rules(at: &str, rules: &[NetworkRule]) -> Result<(), SpecError> {
    for (j, r) in rules.iter().enumerate() {
        let here = format!("{at}[{j}]");
        if let Peer::Domain(d) = &r.peer
            && !is_domain(d)
        {
            return Err(SpecError::new(&here, "the peer domain is not a DNS name"));
        }
        if r.ports.iter().any(|p| p.first == 0 || p.first > p.last) {
            return Err(SpecError::new(
                &format!("{here}.ports"),
                "ranges are 1 to 65535, first <= last",
            ));
        }
    }
    Ok(())
}

fn check_endpoint(i: usize, e: &EndpointSpec) -> Result<(), SpecError> {
    let at = |f: &str| format!("endpoints[{i}].{f}");
    check_name(&at("name"), &e.name)?;
    if e.container_port == 0 {
        return Err(SpecError::new(&at("container_port"), "1 to 65535"));
    }
    if e.exposure == Exposure::Ingress {
        check_ingress(&at, e)?;
    } else if !e.domains.is_empty() {
        let why = "names are routed by the ingress: an endpoint of another exposure is reached \
                   at its address (§3.2 of docs/07-networking.md)";
        return Err(SpecError::new(&at("domains"), why));
    } else if e.tls.is_some() {
        let why = "certificates are the ingress's, which terminates TLS for its names";
        return Err(SpecError::new(&at("tls"), why));
    }
    if !e.policy.egress.is_empty() {
        return Err(SpecError::new(
            &at("policy.egress"),
            "a workload's outbound connections leave from no endpoint: declare them in `egress`",
        ));
    }
    check_rules(&at("policy.ingress"), &e.policy.ingress)
}

/// The most names one endpoint routes: as many as one ACME certificate holds.
pub const MAX_INGRESS_NAMES: usize = 100;

/// An `ingress` endpoint (§3.2 of docs/07-networking.md): HTTP or HTTPS terminated by the proxy
/// with a certificate from ACME or its organization, or TCP carrying TLS the proxy routes by SNI
/// and passes through unread; at least one exact name, and at most [`MAX_INGRESS_NAMES`].
fn check_ingress(at: &dyn Fn(&str) -> String, e: &EndpointSpec) -> Result<(), SpecError> {
    match (e.protocol, &e.tls) {
        (Protocol::Http | Protocol::Https, None | Some(TlsSpec::Acme))
        | (Protocol::Tcp, Some(TlsSpec::Passthrough)) => {}
        (Protocol::Http | Protocol::Https, Some(TlsSpec::Provided { secret })) => {
            if secret.is_empty() {
                let why = "a provided certificate is named by its identifier";
                return Err(SpecError::new(&at("tls.secret"), why));
            }
        }
        (Protocol::Tcp, _) | (_, Some(TlsSpec::Passthrough)) => {
            let why = "TLS the proxy does not terminate is tcp with tls passthrough, routed by SNI";
            return Err(SpecError::new(&at("tls"), why));
        }
        (Protocol::Udp | Protocol::Quic, _) => {
            let why = "the ingress routes by name, and a UDP datagram carries none";
            return Err(SpecError::new(&at("protocol"), why));
        }
    }
    if e.domains.is_empty() || e.domains.len() > MAX_INGRESS_NAMES {
        let why = format!("an ingress endpoint routes 1 to {MAX_INGRESS_NAMES} names");
        return Err(SpecError::new(&at("domains"), why));
    }
    let mut seen = BTreeSet::new();
    for d in &e.domains {
        if crate::dns::domain::DomainName::parse(d).is_none() {
            let why = format!(
                "{d:?} is not an exact lowercase DNS name; a wildcard is question 11 of §11 of \
                 docs/07-networking.md"
            );
            return Err(SpecError::new(&at("domains"), why));
        }
        if !seen.insert(d) {
            return Err(SpecError::new(&at("domains"), format!("{d:?} is named twice")));
        }
    }
    Ok(())
}

/// Checks every endpoint, that names are unique, and that no name is routed to two.
pub fn validate(endpoints: &[EndpointSpec]) -> Result<(), SpecError> {
    let mut names = BTreeSet::new();
    let mut routed = BTreeSet::new();
    for (i, e) in endpoints.iter().enumerate() {
        check_endpoint(i, e)?;
        if !names.insert(&e.name) {
            return Err(SpecError::new(&format!("endpoints[{i}].name"), "declared twice"));
        }
        if let Some(d) = e.domains.iter().find(|d| !routed.insert(d.as_str())) {
            let why = format!("{d:?} routes to another endpoint: a name routes to one");
            return Err(SpecError::new(&format!("endpoints[{i}].domains"), why));
        }
    }
    Ok(())
}
