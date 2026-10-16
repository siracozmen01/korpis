// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::labels::LabelSelector;
use crate::workload::spec::endpoints::{NetworkPolicy, ShapingSpec};

fn endpoint(name: &str, exposure: Exposure) -> EndpointSpec {
    EndpointSpec {
        name: name.into(),
        protocol: Protocol::Tcp,
        container_port: 5432,
        exposure,
        domains: Vec::new(),
        tls: None,
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }
}

fn body(endpoints: Vec<EndpointSpec>) -> IntentBody {
    IntentBody { endpoints, ..crate::workload::spec::fixture() }
}

fn dependency(target: DependencyTarget) -> Dependency {
    Dependency { name: "db".into(), target, inject: Injection::Both, required: true }
}

#[test]
fn a_workload_is_reached_at_its_one_reachable_endpoint() {
    let one = body(vec![endpoint("pg", Exposure::OverlayOnly), endpoint("x", Exposure::None)]);
    assert_eq!(reachable(&one).map(|e| e.name.as_str()), Ok("pg"));
    let none = body(vec![endpoint("x", Exposure::None)]);
    assert_eq!(reachable(&none).err(), Some(NotATarget::NoEndpoint));
    let two = body(vec![endpoint("a", Exposure::NodePort), endpoint("b", Exposure::OverlayOnly)]);
    assert_eq!(reachable(&two).err(), Some(NotATarget::SeveralEndpoints));
}

#[test]
fn each_target_form_resolves_within_the_project_alone() {
    let pg = body(vec![endpoint("pg", Exposure::OverlayOnly)]);
    let tier = BTreeMap::from([("tier".to_owned(), "db".to_owned())]);
    let none = BTreeMap::new();
    let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let project = [
        ProjectWorkload { id: b, name: "replica", labels: &tier, body: Some(&pg) },
        ProjectWorkload { id: a, name: "primary", labels: &tier, body: Some(&pg) },
        ProjectWorkload { id: c, name: "web", labels: &none, body: Some(&pg) },
    ];
    let by_id = resolve(&dependency(DependencyTarget::Workload(a)), &project, None);
    assert_eq!(by_id.targets.iter().map(|t| t.workload).collect::<Vec<_>>(), [a]);
    assert_eq!((by_id.targets[0].endpoint.as_str(), by_id.targets[0].container_port), ("pg", 5432));
    let elsewhere =
        resolve(&dependency(DependencyTarget::Workload(Uuid::from_u128(9))), &project, None);
    assert!(elsewhere.targets.is_empty(), "a workload outside the project is no target");

    let selector = LabelSelector { match_labels: tier.clone(), expressions: Vec::new() };
    let matched = resolve(&dependency(DependencyTarget::Selector(selector)), &project, None);
    let order: Vec<_> = matched.targets.iter().map(|t| t.workload).collect();
    assert_eq!(order, [a, b], "every match, in the order of their names");

    let placed = Endpoint {
        id: Uuid::from_u128(7),
        workload: a,
        name: "pg".into(),
        protocol: Protocol::Tcp,
        container_port: 5432,
        exposure: Exposure::OverlayOnly,
        node: Uuid::from_u128(8),
        port: None,
        domains: Vec::new(),
        tls: None,
        certificate: None,
    };
    let target = DependencyTarget::Endpoint(placed.id);
    assert_eq!(resolve(&dependency(target.clone()), &project, Some(&placed)).targets.len(), 1);
    let foreign = Endpoint { workload: Uuid::from_u128(9), ..placed.clone() };
    assert!(resolve(&dependency(target), &project, Some(&foreign)).targets.is_empty());
}
