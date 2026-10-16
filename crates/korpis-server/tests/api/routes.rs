// SPDX-License-Identifier: AGPL-3.0-or-later

//! The names `ingress` endpoints route (§3.2 of docs/07-networking.md): only names the
//! workload's organization holds, each by one endpoint in the installation, checked at
//! declaration and claimed when the target moves; freed by a target that drops them (and by
//! retirement, which `korpis-store`'s tests show); and kept from being taken by a domain's
//! deletion or delegation while routed.

use crate::domains::child;
use crate::recipes::{body, change, create, project, push_paper};
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::v1 as pb;

use pb::DomainVerification as V;

/// The example with a `web` endpoint behind the ingress routing `names`.
pub fn web(repo: &str, names: &[&str]) -> pb::IntentBody {
    let web = pb::EndpointSpec {
        name: "web".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP.into(),
        container_port: 8080,
        exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
        domains: names.iter().map(|n| (*n).to_owned()).collect(),
        ..Default::default()
    };
    pb::IntentBody { endpoints: vec![web], ..body(repo, &[]) }
}

pub async fn attest(h: &Harness, key: &str, org: &str, name: &str) -> pb::Domain {
    let req = pb::CreateDomainRequest {
        mutation: mutation(key).into(),
        organization: org.into(),
        name: name.into(),
        verification: V::DOMAIN_VERIFICATION_OPERATOR.into(),
        ..Default::default()
    };
    let made = h.domains(h.root()).create_domain(req).await.unwrap().into_owned();
    made.domain.into_option().unwrap()
}

pub async fn declare(h: &Harness, project: &str, name: &str, b: pb::IntentBody) -> pb::Workload {
    let d = create(name, project, name, b);
    let out = h.workloads(h.root()).declare_intent(d).await.unwrap().into_owned();
    out.workload.into_option().unwrap()
}

pub async fn refused(h: &Harness, project: &str, name: &str, b: pb::IntentBody) -> String {
    let d = create(name, project, name, b);
    reason(&h.workloads(h.root()).declare_intent(d).await.unwrap_err())
}

#[tokio::test]
async fn a_name_is_routed_by_its_holder_alone_and_by_one_endpoint_until_it_lets_go() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "shops").await;
    attest(&h, "d1", &h.root_org(), "example.com").await;

    let shop = declare(&h, &project, "shop", web(&repo, &["shop.example.com"])).await;
    let again = web(&repo, &["www.example.com", "shop.example.com"]);
    assert_eq!(refused(&h, &project, "shop-2", again).await, "endpoint.name_routed");
    let elsewhere = web(&repo, &["shop.example.org"]);
    assert_eq!(refused(&h, &project, "other", elsewhere).await, "endpoint.name_not_held");

    let moved = change("c1", &shop.id, shop.version, web(&repo, &["store.example.com"]));
    h.workloads(h.root()).declare_intent(moved).await.unwrap();
    declare(&h, &project, "shop-2", web(&repo, &["shop.example.com"])).await;
    let back = web(&repo, &["store.example.com"]);
    assert_eq!(refused(&h, &project, "shop-3", back).await, "endpoint.name_routed");
}

#[tokio::test]
async fn a_routed_name_keeps_its_holder_through_deletion_and_delegation() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "shops").await;
    let apex = attest(&h, "d1", &h.root_org(), "example.com").await;
    let www = attest(&h, "d2", &h.root_org(), "www.example.com").await;
    declare(&h, &project, "shop", web(&repo, &["shop.example.com", "www.example.com"])).await;

    let beta = child(&h, "beta").await;
    let delegate = pb::CreateDomainRequest {
        mutation: mutation("d3").into(),
        organization: beta,
        name: "shop.example.com".into(),
        verification: V::DOMAIN_VERIFICATION_DELEGATED.into(),
        parent: Some(apex.id.clone()),
        ..Default::default()
    };
    let e = h.domains(h.root()).create_domain(delegate).await.unwrap_err();
    assert_eq!(reason(&e), "domain.routed", "it would take the name from under the endpoint");

    let delete = |key: &str, id: &str| pb::DeleteDomainRequest {
        mutation: mutation(key).into(),
        id: id.to_owned(),
        ..Default::default()
    };
    let domains = h.domains(h.root());
    let e = domains.delete_domain(delete("x1", &apex.id)).await.unwrap_err();
    assert_eq!(reason(&e), "domain.routed", "shop.example.com would have no holder");
    domains.delete_domain(delete("x2", &www.id)).await.expect("example.com still holds www");
}

#[tokio::test]
async fn an_ingress_endpoint_lands_where_the_ingress_is_served_and_its_node_is_told_the_route() {
    use crate::agent::{TestAgent, welcome};
    use crate::journal::{assignment, until};
    use std::time::{Duration, SystemTime};

    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-1", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "shops").await;
    attest(&h, "d1", &h.root_org(), "example.com").await;
    let w = declare(&h, &project, "shop", web(&repo, &["shop.example.com"])).await;

    let status = || async {
        let get = pb::GetWorkloadRequest { id: w.id.clone(), ..Default::default() };
        let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
        got.status.into_option().unwrap()
    };
    let unplaced = status().await;
    let why = unplaced.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    assert!(why.filtered.iter().any(|f| f.detail.contains("80 and 443")), "{why:?}");

    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = true").execute(store.pool()).await.unwrap();
    let mut endpoints = Vec::new();
    for _ in 0..100 {
        endpoints = status().await.endpoints;
        if !endpoints.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let web = endpoints.iter().find(|e| e.name == "web").expect("placed");
    let address = web.address.as_option().unwrap();
    assert_eq!((address.ips.as_slice(), address.port), (&["203.0.113.10".to_owned()][..], 443));
    assert_eq!(web.domains, ["shop.example.com"]);
    let tls = web.tls.as_option().unwrap().source.as_known();
    assert_eq!(tls, Some(pb::TlsSource::TLS_SOURCE_ACME), "the default, made explicit");
    let translated = Some(pb::ClientAddress::CLIENT_ADDRESS_TRANSLATED);
    assert_eq!(web.client_address.as_known(), translated, "the proxy is the peer");

    let told = until(&mut s, assignment(|a| a.endpoints.iter().any(|e| e.name == "web"))).await;
    let route = told.endpoints.iter().find(|e| e.name == "web").unwrap();
    assert_eq!(route.domains, ["shop.example.com"], "the node's proxy is given the route");
}
