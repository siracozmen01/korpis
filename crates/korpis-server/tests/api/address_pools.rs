// SPDX-License-Identifier: AGPL-3.0-or-later

//! Address pools and dedicated addresses (§3.7 of docs/07-networking.md, trace 77 of
//! docs/23-walkthroughs.md), the control plane's half: a pool is held on its organization's own
//! nodes alone and overlaps no other; a workload with `dedicated_ip` endpoints is placed only
//! where a pool reaches with a free address, holds one however many such endpoints it has, is
//! told it in its assignment and shows it on its endpoints; and a pool is deleted only while none
//! of its addresses is held. Making one is an operator's attestation, since nothing proves who
//! holds a range (Finding 187 of docs/23-walkthroughs.md), and deleting one is its owner's.

use crate::dependencies::{declare, node, placement};
use crate::domains::child;
use crate::egress::assigned;
use crate::recipes::{body, project, push_paper};
use crate::support::{Harness, mutation, org_scope, reason};
use connectrpc::ErrorCode;
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::principal;

fn asked(key: &str, org: &str, cidr: &str, node: &str) -> pb::CreateAddressPoolRequest {
    let pool = pb::AddressPool {
        organization: org.into(),
        cidr: cidr.into(),
        scope: pb::AddressPoolScope::ADDRESS_POOL_SCOPE_NODE.into(),
        node: node.into(),
        interface: "eth0".into(),
        // Its link's gateway.
        reserved: vec!["203.0.113.1".into()],
        ..Default::default()
    };
    pb::CreateAddressPoolRequest {
        mutation: mutation(key).into(),
        pool: pool.into(),
        ..Default::default()
    }
}

async fn made(h: &Harness, req: pb::CreateAddressPoolRequest) -> pb::AddressPool {
    let out = h.pools(h.root()).create_address_pool(req).await.unwrap().into_owned();
    out.pool.into_option().unwrap()
}

async fn refusal(h: &Harness, req: pb::CreateAddressPoolRequest) -> String {
    reason(&h.pools(h.root()).create_address_pool(req).await.unwrap_err())
}

/// A workload whose game and query endpoints are both `dedicated_ip`.
fn arena(repo: &str) -> pb::IntentBody {
    let dedicated = |name: &str| pb::EndpointSpec {
        name: name.into(),
        exposure: pb::Exposure::EXPOSURE_DEDICATED_IP.into(),
        ..Default::default()
    };
    pb::IntentBody { endpoints: vec![dedicated("query"), dedicated("rcon")], ..body(repo, &[]) }
}

async fn status(h: &Harness, workload: &str) -> pb::WorkloadStatus {
    let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

#[tokio::test]
async fn a_pool_s_addresses_are_held_on_its_organization_s_own_nodes_one_to_a_workload() {
    let h = Harness::new().await;
    let reseller = child(&h, "pinecone").await;
    let reseller_id = Id::parse(IdKind::Organization, &reseller).unwrap().uuid();
    // The host's node is connected, so the assignments it is sent are kept.
    let (hosts, _session) = node(&h, "pool-a", 71).await;
    let theirs = h.node(Some(reseller_id), "pool-r", &[]).await;
    let root = h.root_org();

    let on_the_host_s = asked("p0", &reseller, "203.0.113.0/29", &hosts);
    assert_eq!(refusal(&h, on_the_host_s).await, "request.invalid", "a node not its own");
    let pool = made(&h, asked("p1", &root, "203.0.113.0/30", &hosts)).await;
    assert_eq!(pool.routing.as_known(), Some(pb::AddressRouting::ADDRESS_ROUTING_STATIC));
    assert_eq!(pool.held, 0);
    let overlapping = asked("p2", &reseller, "203.0.113.0/29", &theirs);
    assert_eq!(refusal(&h, overlapping).await, "address_pool.overlaps");

    let repo = push_paper().await;
    let games = project(&h, &root, "games").await;
    let first = declare(&h, &games, "first", arena(&repo)).await;
    assert_eq!(placement(&h, &first.id).await.node, hosts, "where the pool reaches");
    let told = assigned(&h, &first.id).await;
    let held = told.dedicated.as_option().expect("its address, in its assignment");
    assert_eq!((held.address.as_str(), held.interface.as_str()), ("203.0.113.2", "eth0"));
    let shown = status(&h, &first.id).await.endpoints;
    let at = |e: &pb::Endpoint| e.address.as_option().map(|a| (a.ips.clone(), a.port));
    let reached: Vec<_> = shown.iter().map(|e| (e.name.as_str(), at(e))).collect();
    let own = |port| Some((vec!["203.0.113.2".to_owned()], port));
    // The recipe's own `game` endpoint is a port of the node's; the two declared here are not.
    assert_eq!(reached[1..], [("query", own(25565)), ("rcon", own(25575))], "{reached:?}");

    let listed = pb::ListAddressPoolsRequest { organization: root.clone(), ..Default::default() };
    let pools = h.pools(h.root()).list_address_pools(listed).await.unwrap().into_owned().pools;
    assert_eq!(pools[0].held, 1, "one address for both of its endpoints");

    // Its other addresses are its gateway, reserved, and its broadcast address, never given.
    let second = declare(&h, &games, "second", arena(&repo)).await;
    let why = status(&h, &second.id).await.unplaced.into_option().unwrap();
    let checks: Vec<String> =
        why.explanation.into_option().unwrap().filtered.into_iter().map(|r| r.check).collect();
    assert!(checks.contains(&"dedicated_ip".to_owned()), "{checks:?}");

    let delete = pb::DeleteAddressPoolRequest {
        mutation: mutation("d1").into(),
        id: pool.id.clone(),
        ..Default::default()
    };
    let held = h.pools(h.root()).delete_address_pool(delete).await.unwrap_err();
    assert_eq!(reason(&held), "address_pool.held");
}

#[tokio::test]
async fn making_a_pool_is_an_operator_s_attestation_and_deleting_it_its_owner_s() {
    let h = Harness::new().await;
    let reseller = child(&h, "pinecone").await;
    let reseller_id = Id::parse(IdKind::Organization, &reseller).unwrap().uuid();
    let theirs = h.node(Some(reseller_id), "pool-r", &[]).await;
    let (tenant, tenant_secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("g1").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(tenant)).into(),
            actions: vec!["address_pool.write".into(), "address_pool.read".into()],
            scope: org_scope(&reseller).into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let tenants = h.pools(h.config(Some(tenant_secret.expose())));
    let squat = asked("t1", &reseller, "203.0.113.0/29", &theirs);
    let e = tenants.create_address_pool(squat).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "a range is vouched for by an operator: {e:?}");

    let (operator, operator_secret) = h.bare_token().await;
    h.issue_to(operator, "g2", &["address_pool.attest"]).await;
    let operators = h.pools(h.config(Some(operator_secret.expose())));
    let made = operators.create_address_pool(asked("o1", &reseller, "203.0.113.0/29", &theirs));
    let pool = made.await.expect("attested for a descendant").into_owned().pool.into_option();
    let pool = pool.unwrap();
    assert_eq!(pool.organization, reseller);
    let delete = pb::DeleteAddressPoolRequest {
        mutation: mutation("t2").into(),
        id: pool.id.clone(),
        ..Default::default()
    };
    tenants.delete_address_pool(delete).await.expect("its own pool, none of it held");
}
