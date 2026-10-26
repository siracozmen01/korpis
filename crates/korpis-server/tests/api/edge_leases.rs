// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases on an edge group's addresses (§3.1 of docs/07-networking.md, trace 78 of
//! docs/23-walkthroughs.md): the control plane names one member in its table to hold a stable
//! address, and grants that member alone the address's lease; another member asking is told who
//! holds it; the holder renews under its epoch, and once it has released the address a lease on
//! it is a new epoch. An address no endpoint holds is nobody's to lease.

use crate::across::running_at;
use crate::agent::{Stream, TestAgent, any};
use crate::dependencies::declare;
use crate::edge_groups::{advertise, arena_body, asked, cordon, edge_pool, edges_once, made};
use crate::issuance::send;
use crate::mesh::{opened, side};
use crate::recipes::{project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;

const ADDRESS: &str = "198.51.100.2";

fn ask(address: &str, nonce: u8, held_epoch: u64) -> Up {
    Up::EdgeLeaseRequest(Box::new(agent::EdgeLeaseRequest {
        address: address.into(),
        nonce: vec![nonce],
        held_epoch,
        ..Default::default()
    }))
}

/// The answer to the edge lease `s` asked for, within five seconds of each message before it.
async fn answer(s: &mut Stream) -> Down {
    loop {
        if let Some(d @ (Down::EdgeLeaseGrant(_) | Down::EdgeLeaseRefusal(_))) =
            any(s).await.message
        {
            return d;
        }
    }
}

/// The epoch `d` grants with nonce `nonce`.
fn granted(d: &Down, nonce: u8) -> u64 {
    let Down::EdgeLeaseGrant(g) = d else { panic!("not granted: {d:?}") };
    assert_eq!((g.address.as_str(), g.nonce.as_slice()), (ADDRESS, &[nonce][..]));
    assert_eq!(g.term.as_option().map(|t| t.seconds), Some(10), "the group's term");
    g.epoch
}

fn refused(d: &Down) -> (agent::LeaseRefusalReason, String) {
    let Down::EdgeLeaseRefusal(r) = d else { panic!("not refused: {d:?}") };
    (r.reason.as_known().unwrap(), r.superseded_by.clone())
}

/// Whether `told` names `ADDRESS` for its member to hold.
fn holds(told: &agent::Edges) -> bool {
    told.groups.iter().flat_map(|g| &g.hold).any(|h| h.address == ADDRESS)
}

#[tokio::test]
async fn the_member_named_holds_the_address_under_a_lease_no_other_member_is_granted() {
    let h = Harness::new().await;
    let (e1, e2, host) = (
        TestAgent::enroll(&h, "edge-1", 91).await,
        TestAgent::enroll(&h, "edge-2", 92).await,
        TestAgent::enroll(&h, "host-a", 93).await,
    );
    let mut s1 = opened(&h, &e1, side(1, "10.48.0.0/22", &[2, 3])).await;
    let mut s2 = opened(&h, &e2, side(2, "10.48.4.0/22", &[1, 3])).await;
    let mut sa = opened(&h, &host, side(3, "10.48.8.0/22", &[1, 2])).await;
    cordon(&h, &e1.node).await;
    cordon(&h, &e2.node).await;
    advertise(&h, &e1.node, "127.0.0.91").await;
    advertise(&h, &e2.node, "127.0.0.92").await;
    let root = h.root_org();
    // Probed once a minute, the members stay unprobed, connected, for the whole test.
    let mut slow = asked("g1", &root, "edge", &[&e1.node, &e2.node]);
    let mut g = slow.group.as_option().cloned().unwrap();
    g.probe_interval = korpis_wire::duration(std::time::Duration::from_secs(60)).into();
    slow.group = g.into();
    let group = made(&h, slow).await;
    h.pools(h.root())
        .create_address_pool(edge_pool("p1", &root, "198.51.100.0/29", &group.id))
        .await
        .unwrap();
    let games = project(&h, &root, "games").await;
    let arena = declare(&h, &games, "arena", arena_body(&push_paper().await)).await;
    running_at(&mut sa, &arena.id, "10.48.8.2").await;

    let forwarding = |e: &agent::Edges| e.groups.first().is_some_and(|g| !g.forwards.is_empty());
    let (told1, told2) =
        (edges_once(&mut s1, forwarding).await, edges_once(&mut s2, forwarding).await);
    assert!(holds(&told1) != holds(&told2), "one member, and one only, is named to hold it");
    let ((holder, held), (other, _)) = if holds(&told1) {
        ((&mut s1, &e1), (&mut s2, &e2))
    } else {
        ((&mut s2, &e2), (&mut s1, &e1))
    };

    send(holder, ask(ADDRESS, 1, 0)).await;
    let epoch = granted(&answer(holder).await, 1);
    send(other, ask(ADDRESS, 2, 0)).await;
    let superseded =
        (agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_SUPERSEDED, held.node.clone());
    assert_eq!(refused(&answer(other).await), superseded, "told who holds it");
    send(holder, ask(ADDRESS, 3, epoch)).await;
    assert_eq!(granted(&answer(holder).await, 3), epoch, "renewed under its epoch");

    let released =
        agent::EdgeLeaseReleased { address: ADDRESS.into(), epoch, ..Default::default() };
    send(holder, Up::EdgeLeaseReleased(Box::new(released))).await;
    send(holder, ask(ADDRESS, 4, 0)).await;
    assert!(granted(&answer(holder).await, 4) > epoch, "released, it is a new epoch");

    send(&mut sa, ask("198.51.100.3", 5, 0)).await;
    let nobody = refused(&answer(&mut sa).await).0;
    assert_eq!(nobody, agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_NOT_ASSIGNED);
}
