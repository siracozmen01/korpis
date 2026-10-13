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
    /// Provided by the tenant, held in a secret.
    Provided {
        /// The secret holding the certificate and key.
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

/// An endpoint's network policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkPolicy {
    /// Inbound rules, in order.
    pub ingress: Vec<NetworkRule>,
    /// Outbound rules, in order.
    pub egress: Vec<NetworkRule>,
    /// Connection-tracking limits.
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

fn check_rules(at: &str, rules: &[NetworkRule]) -> Result<(), SpecError> {
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
    let http = matches!(e.protocol, Protocol::Http | Protocol::Https);
    if !e.domains.is_empty() && !http {
        return Err(SpecError::new(&at("domains"), "domains route HTTP and HTTPS only"));
    }
    if let Some(bad) = e.domains.iter().find(|d| !is_domain(d)) {
        return Err(SpecError::new(&at("domains"), format!("{bad:?} is not a DNS name")));
    }
    match &e.tls {
        Some(_) if e.protocol != Protocol::Https => {
            Err(SpecError::new(&at("tls"), "certificates are for HTTPS endpoints"))
        }
        Some(TlsSpec::Provided { secret }) if secret.is_empty() => {
            Err(SpecError::new(&at("tls.secret"), "a provided certificate names its secret"))
        }
        _ => Ok(()),
    }?;
    check_rules(&at("policy.ingress"), &e.policy.ingress)?;
    check_rules(&at("policy.egress"), &e.policy.egress)
}

/// Checks every endpoint and that names are unique.
pub fn validate(endpoints: &[EndpointSpec]) -> Result<(), SpecError> {
    let mut names = BTreeSet::new();
    for (i, e) in endpoints.iter().enumerate() {
        check_endpoint(i, e)?;
        if !names.insert(&e.name) {
            return Err(SpecError::new(&format!("endpoints[{i}].name"), "declared twice"));
        }
    }
    Ok(())
}
