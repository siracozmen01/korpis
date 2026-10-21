// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's own network policy, from its target body to what the helper applies (§6 of
//! docs/07-networking.md): its egress rules and limits, and each endpoint's rules and limits on
//! who connects to it. The control plane refuses at declaration every rule a node cannot hold;
//! one that arrives anyway fails the workload's network rather than being left out, because a
//! deny left out is a weaker policy than the one declared.

use super::act::Failure;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::network_rule::Peer;

fn transport(p: buffa::EnumValue<pb::EndpointProtocol>) -> helper::Transport {
    match p.as_known() {
        Some(
            pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP
            | pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC,
        ) => helper::Transport::TRANSPORT_UDP,
        Some(pb::EndpointProtocol::ENDPOINT_PROTOCOL_UNSPECIFIED) | None => {
            helper::Transport::TRANSPORT_UNSPECIFIED
        }
        _ => helper::Transport::TRANSPORT_TCP,
    }
}

fn rule(r: &pb::NetworkRule) -> Result<helper::Rule, Failure> {
    let Some(Peer::Cidr(cidr)) = &r.peer else {
        return Err(Failure("a network rule names a peer no node can judge by address".into()));
    };
    let allow = match r.action.as_known() {
        Some(pb::NetworkAction::NETWORK_ACTION_ALLOW) => true,
        Some(pb::NetworkAction::NETWORK_ACTION_DENY) => false,
        _ => return Err(Failure("a network rule neither allows nor denies".into())),
    };
    let ports = r
        .ports
        .iter()
        .map(|p| helper::PortSpan { first: p.first, last: p.last, ..Default::default() })
        .collect();
    Ok(helper::Rule {
        allow,
        cidr: cidr.clone(),
        ports,
        transport: transport(r.protocol).into(),
        ..Default::default()
    })
}

fn rules(rules: &[pb::NetworkRule]) -> Result<Vec<helper::Rule>, Failure> {
    rules.iter().map(rule).collect()
}

/// What `body` may send and be sent, as the helper holds it (§7 of docs/07-networking.md): an
/// egress reservation with no limit is its own limit, as placement counts it; none where it
/// sets nothing.
fn bandwidth(body: &pb::IntentBody) -> Option<helper::Bandwidth> {
    let b = body.resources.as_option()?.bandwidth.as_option()?;
    let egress = b.egress.as_option().cloned().unwrap_or_default();
    let limit = if egress.limit > 0 { egress.limit } else { egress.reservation };
    (limit > 0 || b.ingress_limit > 0).then(|| helper::Bandwidth {
        egress_reservation: egress.reservation,
        egress_limit: limit,
        ingress_limit: b.ingress_limit,
        ..Default::default()
    })
}

/// Holds the workload `request` makes the network of to what a spent egress budget says,
/// where `shaping` says one is spent (§5.1 of docs/05-scheduling.md): its floor in place of its
/// limit, or nothing leaving its link for beyond the node.
pub(super) fn spent(request: &mut helper::EnsureNetworkRequest, shaping: Option<&agent::Shaping>) {
    let Some(s) = shaping.filter(|s| s.egress_exhausted) else { return };
    let mut b = request.bandwidth.as_option().cloned().unwrap_or_default();
    (b.exhausted, b.floor, b.cut_off) = (true, s.egress_floor_bits_per_sec, s.egress_cut_off);
    request.bandwidth = b.into();
}

/// Sets on `request` the workload's own outbound rules and limits, and on each of its inbound
/// ports the rules and limits of the endpoint listening there, from `body`.
pub(super) fn apply(
    request: &mut helper::EnsureNetworkRequest,
    body: Option<&pb::IntentBody>,
) -> Result<(), Failure> {
    let Some(body) = body else { return Ok(()) };
    request.bandwidth = bandwidth(body).into();
    if let Some(egress) = body.egress.as_option() {
        request.egress = rules(&egress.rules)?;
        if let Some(l) = egress.limits.as_option() {
            (request.max_connections, request.new_per_sec) = (l.max_connections, l.new_per_sec);
        }
    }
    for port in &mut request.inbound {
        let declared = body.endpoints.iter().filter(|e| e.container_port == port.port);
        let declared = declared
            .filter(|e| transport(e.protocol) == port.transport.as_known().unwrap_or_default());
        for e in declared {
            let Some(policy) = e.policy.as_option() else { continue };
            port.rules.extend(rules(&policy.ingress)?);
            if let Some(c) = policy.conntrack.as_option() {
                port.max_connections = nonzero_min(port.max_connections, c.max_connections);
                port.new_per_sec = nonzero_min(port.new_per_sec, c.new_per_sec);
            }
        }
    }
    Ok(())
}

/// The tighter of two limits where zero is none.
fn nonzero_min(a: u32, b: u32) -> u32 {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (a, b) => a.min(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limit_of_zero_is_none_and_two_take_the_tighter() {
        assert_eq!((nonzero_min(0, 5), nonzero_min(7, 0), nonzero_min(7, 5)), (5, 7, 5));
    }

    #[test]
    fn a_rule_naming_a_peer_by_label_fails_rather_than_being_left_out() {
        let r = pb::NetworkRule {
            action: pb::NetworkAction::NETWORK_ACTION_DENY.into(),
            peer: Some(Peer::Domain("example.org".into())),
            ..Default::default()
        };
        assert!(rule(&r).is_err());
        let r = pb::NetworkRule { peer: Some(Peer::Cidr("10.1.0.0/16".into())), ..r };
        let made = rule(&r).ok().map(|h| (h.allow, h.cidr));
        assert_eq!(made, Some((false, "10.1.0.0/16".to_owned())));
    }

    #[test]
    fn a_spent_budget_holds_the_workload_beside_what_it_asks_for_itself() {
        let mut request = helper::EnsureNetworkRequest::default();
        spent(&mut request, Some(&agent::Shaping::default()));
        assert!(request.bandwidth.as_option().is_none(), "nothing spent, nothing held");
        request.bandwidth =
            helper::Bandwidth { egress_limit: 50_000_000, ..Default::default() }.into();
        let floor = agent::Shaping {
            egress_exhausted: true,
            egress_floor_bits_per_sec: 1_000_000,
            ..Default::default()
        };
        spent(&mut request, Some(&floor));
        let b = request.bandwidth.as_option().cloned().unwrap_or_default();
        assert_eq!(
            (b.exhausted, b.floor, b.cut_off, b.egress_limit),
            (true, 1_000_000, false, 50_000_000)
        );
    }
}
