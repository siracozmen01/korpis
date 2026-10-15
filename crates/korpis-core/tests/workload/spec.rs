// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declaration's own consistency.

use crate::support::body;
use korpis_core::workload::spec::endpoints::{Exposure, Protocol, TlsSpec};
use korpis_core::workload::spec::health::{Probe, ProbeKind};
use korpis_core::workload::spec::{ConsoleMode, Lifecycle};

fn refused(mutate: impl FnOnce(&mut korpis_core::workload::IntentBody)) -> String {
    let mut b = body();
    mutate(&mut b);
    b.validate().expect_err("refused").field
}

/// The example's endpoint behind the ingress, as `protocol` with `tls`, routing `names`.
fn ingress(
    protocol: Protocol,
    tls: Option<TlsSpec>,
    names: &[&str],
) -> impl FnOnce(&mut korpis_core::workload::IntentBody) {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    move |b| {
        let e = &mut b.endpoints[0];
        (e.exposure, e.protocol, e.tls, e.domains) = (Exposure::Ingress, protocol, tls, names);
    }
}

#[test]
fn an_ingress_endpoint_routes_exact_names_over_http_or_tls_it_passes_through() {
    let valid = |m: Box<dyn FnOnce(&mut korpis_core::workload::IntentBody)>| {
        let mut b = body();
        m(&mut b);
        b.validate()
    };
    let shop = &["shop.example.com"][..];
    assert!(valid(Box::new(ingress(Protocol::Http, None, shop))).is_ok(), "ACME by default");
    assert!(valid(Box::new(ingress(Protocol::Https, Some(TlsSpec::Acme), shop))).is_ok());
    assert!(valid(Box::new(ingress(Protocol::Tcp, Some(TlsSpec::Passthrough), shop))).is_ok());
    assert_eq!(refused(ingress(Protocol::Tcp, None, shop)), "endpoints[0].tls");
    assert_eq!(
        refused(ingress(Protocol::Http, Some(TlsSpec::Passthrough), shop)),
        "endpoints[0].tls"
    );
    assert_eq!(refused(ingress(Protocol::Udp, None, shop)), "endpoints[0].protocol");
    let provided = Some(TlsSpec::Provided { secret: String::new() });
    assert_eq!(refused(ingress(Protocol::Http, provided, shop)), "endpoints[0].tls.secret");
    assert_eq!(refused(ingress(Protocol::Http, None, &[])), "endpoints[0].domains");
    assert_eq!(refused(ingress(Protocol::Http, None, &["*.example.com"])), "endpoints[0].domains");
    assert_eq!(
        refused(ingress(Protocol::Http, None, &["Shop.example.com"])),
        "endpoints[0].domains"
    );
    assert_eq!(
        refused(ingress(Protocol::Http, None, &["a.example.com", "a.example.com"])),
        "endpoints[0].domains"
    );
    let many: Vec<String> = (0..=100).map(|i| format!("n{i}.example.com")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(refused(ingress(Protocol::Http, None, &many)), "endpoints[0].domains");
    let twice = |b: &mut korpis_core::workload::IntentBody| {
        ingress(Protocol::Http, None, &["shop.example.com"])(b);
        let mut other = b.endpoints[0].clone();
        other.name = "admin".into();
        b.endpoints.push(other);
    };
    assert_eq!(refused(twice), "endpoints[1].domains", "a name routes to one endpoint");
}

#[test]
fn the_example_is_valid_and_each_rule_names_its_field() {
    body().validate().unwrap();
    assert_eq!(refused(|b| b.recipe.digest = "sha256:x".into()), "recipe");
    assert_eq!(refused(|b| b.lifecycle = Lifecycle::Scheduled), "schedule");
    assert_eq!(refused(|b| b.schedule = Some("0 4 * * *".into())), "schedule");
    assert_eq!(refused(|b| b.runtime.driver.clear()), "runtime.driver");
    assert_eq!(refused(|b| b.console = ConsoleMode::Serial), "console");
    assert_eq!(refused(|b| b.resources.memory.reservation = 8 << 30), "resources.memory");
    assert_eq!(refused(|b| b.volumes[0].path = "/data/../etc".into()), "volumes[0].path");
    assert_eq!(refused(|b| b.volumes[0].path = "/.korpis/data".into()), "volumes[0].path");
    assert_eq!(refused(|b| b.volumes.push(b.volumes[0].clone())), "volumes[1].name");
    assert_eq!(refused(|b| b.endpoints[0].container_port = 0), "endpoints[0].container_port");
    assert_eq!(
        refused(|b| b.endpoints[0].domains = vec!["mc.example.org".into()]),
        "endpoints[0].domains"
    );
    assert_eq!(refused(|b| b.endpoints[0].tls = Some(TlsSpec::Acme)), "endpoints[0].tls");
    let probe = |endpoint: &str| Probe {
        kind: ProbeKind::Tcp { endpoint: endpoint.into() },
        period: None,
        timeout: None,
        failures: 0,
        initial_delay: None,
    };
    assert_eq!(refused(|b| b.health.readiness = Some(probe("rcon"))), "health.readiness");
    let mut ok = body();
    ok.health.liveness = Some(probe("game"));
    ok.endpoints[0].exposure = Exposure::Ingress;
    ok.endpoints[0].protocol = Protocol::Https;
    ok.endpoints[0].domains = vec!["mc.example.org".into(), "map.example.org".into()];
    ok.endpoints[0].tls = Some(TlsSpec::Provided { secret: "crt_x".into() });
    ok.lifecycle = Lifecycle::Scheduled;
    ok.schedule = Some("0 4 * * *".into());
    ok.validate().unwrap();
}

#[test]
fn outbound_rules_are_the_workload_s_and_not_an_endpoint_s() {
    use korpis_core::workload::spec::endpoints::{NetworkAction, NetworkRule, Peer, PortRange};
    let rule = |ports| NetworkRule {
        action: NetworkAction::Deny,
        peer: Peer::Cidr("203.0.113.0/24".parse().unwrap()),
        ports,
        protocol: None,
    };
    let on_endpoint = |b: &mut korpis_core::workload::IntentBody| {
        b.endpoints[0].policy.egress = vec![rule(Vec::new())];
    };
    assert_eq!(refused(on_endpoint), "endpoints[0].policy.egress");
    let reversed = vec![PortRange { first: 9, last: 1 }];
    assert_eq!(refused(|b| b.egress.rules = vec![rule(reversed)]), "egress.rules[0].ports");
    let mut ok = body();
    ok.egress.rules = vec![rule(vec![PortRange { first: 25, last: 25 }])];
    ok.validate().unwrap();
}
