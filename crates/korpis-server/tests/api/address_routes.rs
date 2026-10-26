// SPDX-License-Identifier: AGPL-3.0-or-later

//! The router's contract (§3.7 of docs/07-networking.md, trace 77 of docs/23-walkthroughs.md): a
//! cluster pool names an `address_router` provider its organization sees, and is refused naming
//! none; a workload holding one of its addresses is placed on any node of the organization, and
//! its node is given the address only once the router, written with the SDK, says it routes it
//! there; a router failing leaves the workload waiting, its status naming the router and why,
//! until it is asked again; and a retired workload's address is routed nowhere.

use crate::dependencies::{declare, placement};
use crate::egress::assigned;
use crate::extensions::{DNS, push_document};
use crate::providers::{Installed, installed_from};
use crate::recipes::{body, project, push_paper};
use crate::support::{Harness, mutation, reason};
use korpis_app::ports::{AddressWrite as _, Store as _, Tx as _};
use korpis_extension::address::{AddressRouter, Failure, Router};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// The network, as the router sees it: every route it was asked for, and whether it fails.
#[derive(Default, Clone)]
pub(crate) struct Fabric(Arc<Mutex<Vec<pb::RouteAddress>>>, pub(crate) Arc<AtomicBool>);

impl Fabric {
    pub(crate) fn routes(&self) -> Vec<(String, String)> {
        let asked = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        asked.iter().map(|r| (r.address.clone(), r.node.clone())).collect()
    }
}

impl AddressRouter for Fabric {
    fn route(&self, r: pb::RouteAddress) -> impl Future<Output = Result<(), Failure>> + Send {
        let down = self.1.load(Ordering::SeqCst);
        if !down {
            self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(r);
        }
        let failure =
            Failure { reason: "bgp.session_down".into(), message: String::new(), retryable: true };
        std::future::ready(if down { Err(failure) } else { Ok(()) })
    }
}

/// A router named `bgp`, installed in the root organization and serving `fabric`; the
/// installation, whose node's session is kept for as long as it is.
pub(crate) async fn serving(h: &Harness, fabric: Fabric) -> Installed {
    let document = DNS
        .replace("    - {type: dns, name: cloudflare}", "    - {type: address_router, name: bgp}")
        .replace(
            "requires: [provider.serve, provider.dns,",
            "requires: [provider.serve, provider.address_router,",
        );
    let x = installed_from(h, 81, "routers", push_document(&document).await).await;
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_ADDRESS_ROUTER,
        name: "bgp".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
    };
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let provider = Arc::new(Provider::new(connection, registration, Router(fabric)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
    x
}

pub(crate) fn cluster(key: &str, org: &str) -> pb::CreateAddressPoolRequest {
    let pool = pb::AddressPool {
        organization: org.into(),
        cidr: "203.0.113.0/29".into(),
        scope: pb::AddressPoolScope::ADDRESS_POOL_SCOPE_CLUSTER.into(),
        router: "bgp".into(),
        routing: pb::AddressRouting::ADDRESS_ROUTING_BGP.into(),
        reserved: vec!["203.0.113.1".into()],
        ..Default::default()
    };
    pb::CreateAddressPoolRequest {
        mutation: mutation(key).into(),
        pool: pool.into(),
        ..Default::default()
    }
}

fn arena(repo: &str) -> pb::IntentBody {
    let game = pb::EndpointSpec {
        name: "query".into(),
        exposure: pb::Exposure::EXPOSURE_DEDICATED_IP.into(),
        ..Default::default()
    };
    pb::IntentBody { endpoints: vec![game], ..body(repo, &[]) }
}

/// `workload`'s dedicated address as its status shows it, once `until` holds of it.
pub(crate) async fn shown(
    h: &Harness,
    workload: &str,
    until: impl Fn(&pb::DedicatedAddressStatus) -> bool,
) -> pb::DedicatedAddressStatus {
    let mut last = pb::DedicatedAddressStatus::default();
    for _ in 0..200 {
        let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
        let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
        last = got.status.into_option().unwrap().dedicated.into_option().unwrap_or_default();
        if until(&last) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("its address never showed as asked: {last:?}");
}

#[tokio::test]
async fn a_cluster_pool_s_address_is_held_where_its_router_says_it_routes_it() {
    let h = Harness::new().await;
    let root = h.root_org();
    let unrouted = h.pools(h.root()).create_address_pool(cluster("p0", &root)).await.unwrap_err();
    assert_eq!(reason(&unrouted), "request.invalid", "no router of that name is installed");
    let fabric = Fabric::default();
    let _router = serving(&h, fabric.clone()).await;
    h.pools(h.root()).create_address_pool(cluster("p1", &root)).await.unwrap();

    let repo = push_paper().await;
    let games = project(&h, &root, "games").await;
    let first = declare(&h, &games, "first", arena(&repo)).await;
    let node = placement(&h, &first.id).await.node;
    let routed = shown(&h, &first.id, |d| !d.waiting_on_router && !d.address.is_empty()).await;
    assert_eq!(routed.address, "203.0.113.2");
    assert_eq!(fabric.routes(), [("203.0.113.2".to_owned(), node)], "asked for its node");
    let told = assigned(&h, &first.id).await;
    assert_eq!(told.dedicated.as_option().map(|d| d.address.as_str()), Some("203.0.113.2"));

    fabric.1.store(true, Ordering::SeqCst);
    let second = declare(&h, &games, "second", arena(&repo)).await;
    let waiting = shown(&h, &second.id, |d| !d.reason.is_empty()).await;
    assert!(waiting.waiting_on_router, "{waiting:?}");
    assert!(waiting.reason.starts_with("router bgp: bgp.session_down"), "{waiting:?}");
    assert!(assigned(&h, &second.id).await.dedicated.is_unset(), "no node holds it yet");
    fabric.1.store(false, Ordering::SeqCst);
    let back = shown(&h, &second.id, |d| !d.waiting_on_router).await;
    assert_eq!((back.address.as_str(), back.reason.as_str()), ("203.0.113.3", ""));
    withdrawn(&h, &fabric, &first.id).await;
}

/// That the address `workload` holds, given back as a retirement gives it back, is routed
/// nowhere.
async fn withdrawn(h: &Harness, fabric: &Fabric, workload: &str) {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    let store = h.db.store().await.unwrap();
    let mut tx = store.begin().await.unwrap();
    assert!(tx.release_address(id.uuid(), SystemTime::now()).await.unwrap());
    tx.commit().await.unwrap();
    let nowhere = ("203.0.113.2".to_owned(), String::new());
    for _ in 0..200 {
        if fabric.routes().contains(&nowhere) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("a released address is routed nowhere: {:?}", fabric.routes());
}
