// SPDX-License-Identifier: AGPL-3.0-or-later

//! Routes (migration 0055, §3.2 of docs/07-networking.md): a name routes to one workload's
//! endpoint, a workload's claim replaces what it routed before, and retiring it frees its names.

use crate::support::{create, fixture, now};
use korpis_app::ports::{DomainRead, DomainWrite, RetirementWrite, Store, Tx};
use korpis_core::dns::domain::{DomainName, Route};
use uuid::Uuid;

fn route(organization: Uuid, workload: Uuid, endpoint: &str, name: &str) -> Route {
    let name = DomainName::parse(name).unwrap();
    Route { name, organization, workload, endpoint: endpoint.into(), certificate: None }
}

fn names(routes: &[Route]) -> Vec<&str> {
    routes.iter().map(|r| r.name.as_str()).collect()
}

#[tokio::test]
async fn a_name_routes_to_one_workload_until_it_lets_go_or_retires() {
    let f = fixture().await;
    let (a, _) = create(&f, "a").await;
    let (b, _) = create(&f, "b").await;
    let org = f.organization;
    let shop = route(org, a.id, "web", "shop.example.com");
    let www = route(org, a.id, "web", "www.example.com");

    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.claim_routes(a.id, &[shop.clone(), www.clone()], now()).await.unwrap(), None);
    assert_eq!(tx.claim_routes(a.id, &[shop.clone(), www.clone()], now()).await.unwrap(), None);
    tx.commit().await.unwrap();
    let taken = route(org, b.id, "web", "shop.example.com");
    let mut tx = f.store.begin().await.unwrap();
    let refused = tx.claim_routes(b.id, std::slice::from_ref(&taken), now()).await.unwrap();
    assert_eq!(refused.as_ref().map(DomainName::as_str), Some("shop.example.com"));
    drop(tx);

    let mut tx = f.store.begin().await.unwrap();
    let admin = route(org, a.id, "admin", "www.example.com");
    assert_eq!(tx.claim_routes(a.id, &[admin], now()).await.unwrap(), None, "shop is let go");
    assert_eq!(tx.claim_routes(b.id, &[taken], now()).await.unwrap(), None);
    tx.commit().await.unwrap();
    let all = [shop.name.clone(), www.name.clone()];
    let held = f.store.routes_of(&all).await.unwrap();
    assert_eq!(names(&held), ["shop.example.com", "www.example.com"]);
    assert_eq!((held[0].workload, held[1].workload), (b.id, a.id));
    assert_eq!(held[1].endpoint, "admin", "pointed at the endpoint naming it now");
    let apex = DomainName::parse("example.com").unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(names(&tx.routes_beneath(&apex).await.unwrap()).len(), 2);

    assert!(tx.retire_workload(a.id, now()).await.unwrap());
    tx.commit().await.unwrap();
    let left = f.store.routes_of(&all).await.unwrap();
    assert_eq!(names(&left), ["shop.example.com"], "retiring frees what it routed");
}
