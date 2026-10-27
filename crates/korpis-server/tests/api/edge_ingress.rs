// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ingress` through an edge group (§3.1 of docs/07-networking.md, trace 78 of
//! docs/23-walkthroughs.md), the control plane's half. A workload whose `ingress` endpoint a group
//! of two members serving the ingress serves is placed on a node that serves none, its names
//! resolving to the group's ingress address, whose ports 80 and 443 a `stable` endpoint is never
//! given; every member is told the group's ingress address and the route, the node behind each
//! member's proxy, through the mesh; a member is given the certificate it serves the names
//! with; and a certificate ordered for them is not looked at by the directory until every
//! connected member has said its proxy holds the challenge.

use crate::across::running_at;
use crate::agent::{Stream, TestAgent};
use crate::certificates::{fetch, issued, served, upload};
use crate::dependencies::placement;
use crate::edge_groups::{arena_body, asked, cordon, edge_pool, edges_once, endpoints_shown, made};
use crate::edge_probes::group_once;
use crate::issuance::send;
use crate::mesh::{opened, side};
use crate::recipes::{project, push_paper};
use crate::routes::{attest, declare, web};
use crate::support::Harness;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::certificate_key::Outcome;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// The group's ingress address: the lowest of its pool, the pool's gateway reserved.
const INGRESS: &str = "198.51.100.2";

/// Two members serving the ingress and a node serving none, each with a session reporting the
/// mesh; the members cordoned, so workloads land on the node.
struct Edged {
    sessions: [Stream; 2],
    host: TestAgent,
    behind: Stream,
    group: pb::EdgeGroup,
}

async fn edged(h: &Harness, seed: u8) -> Edged {
    let (e1, e2, host) = (
        TestAgent::enroll(h, "edge-1", seed).await,
        TestAgent::enroll(h, "edge-2", seed + 1).await,
        TestAgent::enroll(h, "host-a", seed + 2).await,
    );
    let s1 = opened(h, &e1, side(1, "10.48.0.0/22", &[2, 3])).await;
    let s2 = opened(h, &e2, side(2, "10.48.4.0/22", &[1, 3])).await;
    let behind = opened(h, &host, side(3, "10.48.8.0/22", &[1, 2])).await;
    cordon(h, &e1.node).await;
    cordon(h, &e2.node).await;
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET ingress = (name LIKE 'edge-%')")
        .execute(store.pool())
        .await
        .unwrap();
    let root = h.root_org();
    let group = made(h, asked("g1", &root, "edge", &[&e1.node, &e2.node])).await;
    let pool = edge_pool("p1", &root, "198.51.100.0/29", &group.id);
    h.pools(h.root()).create_address_pool(pool).await.unwrap();
    attest(h, "d1", &root, "example.com").await;
    Edged { sessions: [s1, s2], host, behind, group }
}

/// The names `workload`'s endpoints resolve to in DNS, once they resolve to `want`.
async fn resolves(h: &Harness, workload: &str, want: &[&str]) {
    let store = h.db.store().await.unwrap();
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    for _ in 0..600 {
        let wanted: Option<Vec<String>> =
            sqlx::query_scalar("SELECT wanted FROM endpoint_record WHERE workload_id = $1")
                .bind(id.uuid())
                .fetch_optional(store.pool())
                .await
                .unwrap()
                .flatten();
        if wanted.as_deref().is_some_and(|w| w == want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("its names never resolved to {want:?}");
}

#[tokio::test]
async fn an_ingress_endpoint_a_group_serves_is_routed_by_every_member_at_its_ingress_address() {
    let h = Harness::new().await;
    let Edged { sessions: [mut s1, _s2], host, mut behind, group, .. } = edged(&h, 111).await;
    let shops = project(&h, &h.root_org(), "shops").await;
    let (chain, key) = issued(&["shop.example.com"]);
    let c = upload(&h, "c1", &h.root_org(), (chain, key.clone())).await;
    let body = served(&push_paper().await, &["shop.example.com"], &c.id);
    let shop = declare(&h, &shops, "shop", body).await;
    assert_eq!(placement(&h, &shop.id).await.node, host.node, "no node of its own serves it");

    let web = endpoints_shown(&h, &shop.id).await.into_iter().find(|e| e.name == "web").unwrap();
    let at = web.address.as_option().unwrap();
    assert_eq!((at.ips.as_slice(), at.port), (&[INGRESS.to_owned()][..], 443));
    assert_eq!(at.edge_group, group.id, "its group named");
    let shown = group_once(&h, &group.id, |g| !g.ingress_addresses.is_empty()).await;
    assert_eq!(shown.ingress_addresses, [INGRESS]);
    resolves(&h, &shop.id, &[INGRESS]).await;

    running_at(&mut behind, &shop.id, "10.48.8.2").await;
    let routed = |t: &agent::Edges| {
        t.groups.iter().flat_map(|g| &g.proxied).any(|p| !p.workload_address.is_empty())
    };
    let told = edges_once(&mut s1, routed).await;
    let g = &told.groups[0];
    assert_eq!(g.ingress, [INGRESS]);
    let route = &g.proxied[0];
    let endpoint = route.endpoint.as_option().unwrap();
    assert_eq!(endpoint.domains, ["shop.example.com"]);
    assert_eq!((route.workload_address.as_str(), route.node.as_str()), ("10.48.8.2", &*host.node));
    match fetch(&mut s1, "r1", &c.id).await {
        Outcome::Certificate(given) => assert_eq!(given.key, key, "a member serves its names"),
        Outcome::Refused(e) => panic!("a member was refused its group's certificate: {e:?}"),
    }

    let proxies = |t: &agent::Edges| t.sources.iter().filter(|s| !s.proxied.is_empty()).count();
    let told = edges_once(&mut behind, |t| proxies(t) == 2).await;
    for s in &told.sources {
        let admitted: Vec<(&str, u32)> =
            s.proxied.iter().map(|a| (a.workload_address.as_str(), a.container_port)).collect();
        assert_eq!(admitted, [("10.48.8.2", 8080)], "each member's proxy, through the mesh");
        // The recipe's own `stable` endpoints come through the GRE link; the route does not.
        let linked = s.admits.iter().any(|a| a.container_port == 8080);
        assert!(!linked, "the route's port through the mesh alone: {:?}", s.admits);
    }
}

#[tokio::test]
async fn a_stable_endpoint_is_never_given_the_ingress_address_s_own_ports() {
    let h = Harness::new().await;
    // Every session stays open, so the node stays connected and the members with it.
    let Edged { group, sessions: _sessions, behind: _behind, .. } = edged(&h, 114).await;
    let shops = project(&h, &h.root_org(), "shops").await;
    let shop = declare(&h, &shops, "shop", web(&push_paper().await, &["shop.example.com"])).await;
    endpoints_shown(&h, &shop.id).await;
    group_once(&h, &group.id, |g| g.ingress_addresses == [INGRESS]).await;
    // The group gives `stable` endpoints port 443 alone from now on.
    let store = h.db.store().await.unwrap();
    let narrow = "UPDATE edge_group SET first_port = 443, last_port = 443";
    sqlx::query(narrow).execute(store.pool()).await.unwrap();
    // A game's `stable` endpoints, given 443 over TCP and UDP, are given them at the next address.
    let game = declare(&h, &shops, "game", arena_body(&push_paper().await)).await;
    let shown = endpoints_shown(&h, &game.id).await;
    for name in ["game", "query"] {
        let e = shown.iter().find(|e| e.name == name).unwrap();
        let at = e.address.as_option().unwrap();
        assert_eq!((at.ips.as_slice(), at.port), (&["198.51.100.3".to_owned()][..], 443));
    }
}

#[tokio::test]
async fn the_directory_looks_only_once_every_connected_member_holds_the_challenge() {
    let Some(pebble) = crate::pebble::start(true).await else { return };
    let h = Harness::with_acme(pebble.config(), None).await;
    let Edged { sessions: [mut s1, mut s2], behind: _behind, .. } = edged(&h, 117).await;
    let shops = project(&h, &h.root_org(), "shops").await;
    declare(&h, &shops, "shop", web(&push_paper().await, &["shop.example.com"])).await;
    let challenged = |t: &agent::Edges| !t.challenges.is_empty();
    let token = edges_once(&mut s1, challenged).await.challenges[0].token.clone();
    let held = || {
        Up::EdgeChallengesHeld(Box::new(agent::EdgeChallengesHeld {
            tokens: vec![token.clone()],
            ..Default::default()
        }))
    };
    send(&mut s1, held()).await;
    // One member of two holding it is not enough.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let listed = pb::ListCertificatesRequest { organization: h.root_org(), ..Default::default() };
    let certs = h.certificates(h.root());
    let state = |c: &[pb::Certificate]| c.first().and_then(|c| c.state.as_known());
    let pending = certs.list_certificates(listed.clone()).await.unwrap().into_owned();
    let waiting = Some(pb::CertificateState::CERTIFICATE_STATE_PENDING);
    assert_eq!(state(&pending.certificates), waiting, "the other member does not hold it yet");
    send(&mut s2, held()).await;
    for _ in 0..300 {
        let now = certs.list_certificates(listed.clone()).await.unwrap().into_owned();
        if state(&now.certificates) == Some(pb::CertificateState::CERTIFICATE_STATE_ISSUED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the certificate was never issued");
}
