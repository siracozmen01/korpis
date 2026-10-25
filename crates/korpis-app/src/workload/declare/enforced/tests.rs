// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use korpis_core::labels::LabelSelector;
use korpis_core::workload::spec::endpoints::{
    NetworkAction, NetworkPolicy, PortRange, Protocol, ShapingSpec, TlsSpec,
};

fn declared() -> EndpointSpec {
    EndpointSpec {
        name: "game".into(),
        protocol: Protocol::Tcp,
        container_port: 25565,
        exposure: Exposure::NodeBound,
        domains: Vec::new(),
        tls: None,
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }
}

fn rule(action: NetworkAction, peer: Peer) -> NetworkRule {
    NetworkRule { action, peer, ports: Vec::new(), protocol: None }
}

fn cidr(text: &str) -> Peer {
    Peer::Cidr(text.parse().unwrap())
}

fn why(refused: Result<(), AppError>) -> String {
    match refused {
        Err(AppError::NotYet { what, phase }) => format!("{what} in phase {phase}"),
        Err(AppError::Invalid { field, .. }) => field,
        other => panic!("accepted: {other:?}"),
    }
}

fn refused(change: impl FnOnce(&mut EndpointSpec)) -> String {
    let mut e = declared();
    change(&mut e);
    why(endpoint("e", &e))
}

#[test]
fn what_a_node_enforces_is_accepted() {
    let mut e = declared();
    e.policy.ingress = vec![
        rule(NetworkAction::Allow, cidr("5.6.7.0/24")),
        rule(NetworkAction::Deny, cidr("0.0.0.0/0")),
    ];
    e.policy.conntrack.max_connections = 8192;
    e.policy.conntrack.new_per_sec = 50;
    assert!(endpoint("e", &e).is_ok());
    let mut ported = rule(NetworkAction::Deny, cidr("1.2.3.0/24"));
    ported.ports = vec![PortRange { first: 1, last: 1024 }];
    ported.protocol = Some(Protocol::Udp);
    for r in [&ported, &rule(NetworkAction::Allow, cidr("2001:db8::/32"))] {
        assert!(peer("r", r).is_ok());
    }
}

#[test]
fn what_no_node_of_this_release_enforces_is_refused_with_where_it_is_built() {
    assert_eq!(refused(|e| e.exposure = Exposure::DedicatedIp), "dedicated addresses in phase 3");
    let mut ingress = declared();
    (ingress.exposure, ingress.tls) = (Exposure::Ingress, Some(TlsSpec::Acme { clients: None }));
    ingress.domains = vec!["shop.example.com".into()];
    assert!(super::endpoint("e", &ingress).is_ok(), "the ingress is built");
    ingress.tls = Some(TlsSpec::Provided { secret: "crt_x".into(), clients: None });
    assert!(super::endpoint("e", &ingress).is_ok(), "so are provided certificates");
    let mut shaped = declared();
    (shaped.shaping.egress_bits_per_sec, shaped.accounting) = (1_000_000, true);
    assert!(super::endpoint("e", &shaped).is_ok(), "placed where a node enforces it");
    let half = "e.policy.conntrack.max_half_open";
    assert_eq!(refused(|e| e.policy.conntrack.max_half_open = 10), half);
    let mut ported = rule(NetworkAction::Deny, cidr("5.6.7.0/24"));
    ported.ports = vec![PortRange { first: 1, last: 2 }];
    assert_eq!(refused(|e| e.policy.ingress = vec![ported]), "e.policy.ingress[0]");
    let named = rule(NetworkAction::Deny, Peer::Workloads(LabelSelector::default()));
    assert_eq!(refused(|e| e.policy.ingress = vec![named]), "rules naming workloads in phase 3");
    let domain = rule(NetworkAction::Deny, Peer::Domain("example.org".into()));
    assert_eq!(why(peer("r", &domain)), "r.peer");
}

#[test]
fn a_workload_s_own_limits_lower_the_default_and_never_raise_it() {
    let lowered = ConntrackLimits { max_connections: 100, new_per_sec: 10, max_half_open: 0 };
    assert!(limits(lowered).is_ok());
    let raised = ConntrackLimits { max_connections: DEFAULT_MAX_CONNECTIONS + 1, ..lowered };
    assert_eq!(why(limits(raised)), "body.egress.limits");
    let raised = ConntrackLimits { new_per_sec: DEFAULT_NEW_PER_SEC + 1, ..lowered };
    assert_eq!(why(limits(raised)), "body.egress.limits");
}

#[test]
fn dedicated_cores_are_refused_until_a_driver_holds_them() {
    let mut r = ResourceSpec::default();
    assert!(resources(&r).is_ok());
    r.cpu.isolation = CpuIsolation::DedicatedCores;
    assert_eq!(why(resources(&r)), "dedicated cores in phase 3");
}

#[test]
fn devices_are_refused_until_a_node_gives_them() {
    let mut body = korpis_core::workload::spec::fixture();
    assert!(devices(&body).is_ok());
    body.devices = vec![korpis_core::workload::spec::devices::DeviceRequest {
        class: korpis_core::node::driver::DeviceClass::Gpu,
        count: 1,
        selector: None,
        mode: korpis_core::node::device::DeviceMode::Passthrough,
    }];
    assert_eq!(why(devices(&body)), "devices in phase 3");
}
