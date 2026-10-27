// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declaration's own consistency.

use crate::support::body;
use korpis_core::workload::spec::endpoints::{
    ClientAuth, ClientCertificates, Exposure, Protocol, TlsSpec,
};
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
    assert!(
        valid(Box::new(ingress(Protocol::Https, Some(TlsSpec::Acme { clients: None }), shop)))
            .is_ok()
    );
    for proxy_protocol in [false, true] {
        let passthrough = Some(TlsSpec::Passthrough { proxy_protocol });
        assert!(valid(Box::new(ingress(Protocol::Tcp, passthrough.clone(), shop))).is_ok());
        assert_eq!(refused(ingress(Protocol::Http, passthrough, shop)), "endpoints[0].tls");
    }
    assert_eq!(refused(ingress(Protocol::Tcp, None, shop)), "endpoints[0].tls");
    assert_eq!(refused(ingress(Protocol::Udp, None, shop)), "endpoints[0].protocol");
    let clients = |authorities: Vec<Vec<u8>>| ClientAuth {
        certificates: ClientCertificates::Required,
        authorities,
    };
    let asked = Some(TlsSpec::Acme { clients: Some(clients(vec![vec![0x30; 600]])) });
    assert!(valid(Box::new(ingress(Protocol::Https, asked, shop))).is_ok());
    let trusting_none = Some(TlsSpec::Acme { clients: Some(clients(Vec::new())) });
    assert_eq!(
        refused(ingress(Protocol::Https, trusting_none, shop)),
        "endpoints[0].tls.client_ca"
    );
    let provided = Some(TlsSpec::Provided { secret: String::new(), clients: None });
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
    assert_eq!(
        refused(|b| b.endpoints[0].tls = Some(TlsSpec::Acme { clients: None })),
        "endpoints[0].tls"
    );
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
    ok.endpoints[0].tls = Some(TlsSpec::Provided { secret: "crt_x".into(), clients: None });
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

#[test]
fn a_schedule_its_zone_and_a_run_policy_belong_to_workloads_that_have_runs() {
    use korpis_core::workload::spec::RunPolicy;
    let runs = Some(RunPolicy::default());
    assert_eq!(refused(|b| b.runs = runs), "runs", "a persistent workload has no runs");
    assert_eq!(refused(|b| b.timezone = Some("Europe/Berlin".into())), "timezone");
    let scheduled = |b: &mut korpis_core::workload::IntentBody| {
        b.lifecycle = Lifecycle::Scheduled;
        b.schedule = Some("0 3 * * *".into());
    };
    let mut b = body();
    scheduled(&mut b);
    (b.timezone, b.runs) = (Some("Europe/Berlin".into()), runs);
    assert!(b.validate().is_ok());
    let bad_zone = refused(|b| {
        scheduled(b);
        b.timezone = Some("../etc/localtime".into());
    });
    assert_eq!(bad_zone, "timezone");
    let never = refused(|b| {
        scheduled(b);
        b.schedule = Some("0 3 31 4 *".into());
    });
    assert_eq!(never, "schedule");
    let retries = refused(|b| {
        b.lifecycle = Lifecycle::Task;
        b.runs = Some(RunPolicy { retries: 1000, ..RunPolicy::default() });
    });
    assert_eq!(retries, "runs.retries");
    let overlapping = refused(|b| {
        b.lifecycle = Lifecycle::Task;
        b.runs = Some(RunPolicy {
            concurrency: korpis_core::workload::spec::Concurrency::Allow,
            ..RunPolicy::default()
        });
    });
    assert_eq!(overlapping, "runs.concurrency", "the example declares endpoints");
}

#[test]
fn a_volume_s_replicas_are_one_at_least_a_minute_apart_and_only_on_one_it_makes() {
    use korpis_core::workload::spec::volumes::Replication;
    use std::time::Duration;
    let every = |s| Some(Replication { replicas: 1, interval: Duration::from_secs(s) });
    let mut ok = body();
    ok.volumes[0].replication = every(60);
    assert!(ok.validate().is_ok(), "a minute is enough");
    let field = |r: Option<Replication>| refused(move |b| b.volumes[0].replication = r);
    assert_eq!(field(every(59)), "volumes[0].replication_interval");
    let none = Some(Replication { replicas: 0, interval: Replication::DEFAULT_INTERVAL });
    assert_eq!(field(none), "volumes[0].replicas");
    let attached = refused(|b| {
        b.volumes[0].existing = Some(uuid::Uuid::now_v7());
        b.volumes[0].replication = every(300);
    });
    assert_eq!(attached, "volumes[0].replicas", "an attached volume keeps its own");
}

#[test]
fn failover_onto_a_replica_is_refused_to_a_workload_that_keeps_running() {
    use korpis_core::workload::spec::{Continuity, ReplicaFailover};
    let mut ok = body();
    ok.replica_failover = ReplicaFailover::Automatic;
    assert!(ok.validate().is_ok(), "derived, it stops where it may fail over");
    let kept = refused(|b| {
        b.replica_failover = ReplicaFailover::Automatic;
        b.continuity = Continuity::KeepRunning;
    });
    assert_eq!(kept, "continuity.replica_failover", "it would never be failed over");
}
