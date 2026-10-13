// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases (§4.5 of docs/02-architecture.md): the placed node is granted an epoch and renews it,
//! another node is told who holds it, a release ends it, and the epoch reaches the assignment.

use crate::agent::{Stream, TestAgent, next, welcome};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

async fn connect(h: &Harness, a: &TestAgent, n: u8) -> Stream {
    let (stream, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[n; 16])).await;
    welcome(&first.unwrap());
    stream
}

async fn send(stream: &mut Stream, message: Up) {
    let m = agent::AgentMessage { message: Some(message), ..Default::default() };
    stream.send(m).await.unwrap();
}

fn request(workload: &str, nonce: u8) -> Up {
    Up::LeaseRequest(Box::new(agent::LeaseRequest {
        workload: workload.to_owned(),
        nonce: vec![nonce; 8],
        ..Default::default()
    }))
}

/// The next message `pick` accepts, skipping the others.
async fn until<T>(stream: &mut Stream, pick: impl Fn(&Down) -> Option<T>) -> T {
    for _ in 0..20 {
        if let Some(found) = next(stream).await.message.as_ref().and_then(&pick) {
            return found;
        }
    }
    panic!("not received within twenty messages");
}

fn grant(d: &Down) -> Option<agent::LeaseGrant> {
    if let Down::LeaseGrant(g) = d { Some((**g).clone()) } else { None }
}

fn refusal(d: &Down) -> Option<agent::LeaseRefusal> {
    if let Down::LeaseRefusal(r) = d { Some((**r).clone()) } else { None }
}

/// The lease epoch of the first assignment of a set, once a set carries `epoch`.
fn epoch_set(epoch: u64) -> impl Fn(&Down) -> Option<u64> {
    move |d| {
        let Down::AssignmentSet(s) = d else { return None };
        let a = agent::Assignment::decode_from_slice(&s.assignments.first()?.assignment).ok()?;
        (a.lease_epoch == epoch).then_some(a.generation)
    }
}

#[tokio::test]
async fn the_placed_node_holds_the_lease_and_another_is_told_who_does() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let mut sa = connect(&h, &a, 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();

    send(&mut sa, request(&workload, 1)).await;
    let first = until(&mut sa, grant).await;
    assert!(first.epoch > 0);
    assert_eq!(first.nonce, vec![1; 8], "the nonce is echoed");
    assert_eq!((first.term.as_option().map(|t| t.seconds), first.rho_ppm), (Some(2), 1000));
    let keep = pb::OnLeaseExpiry::ON_LEASE_EXPIRY_KEEP_RUNNING;
    assert_eq!(first.on_expiry.as_known(), Some(keep), "resolved, never unspecified");
    until(&mut sa, epoch_set(first.epoch)).await;
    send(&mut sa, request(&workload, 2)).await;
    assert_eq!(until(&mut sa, grant).await.epoch, first.epoch, "a renewal keeps the epoch");

    let b = TestAgent::enroll(&h, "fra-4", 2).await;
    let mut sb = connect(&h, &b, 1).await;
    send(&mut sb, request(&workload, 3)).await;
    let refused = until(&mut sb, refusal).await;
    let superseded = agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_SUPERSEDED;
    assert_eq!(refused.reason.as_known(), Some(superseded));
    assert_eq!(refused.superseded_by, a.node, "the refusal names the holder");
    let unknown = "wl_00000000000000000000000000000000";
    send(&mut sb, request(unknown, 4)).await;
    let not_assigned = agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_NOT_ASSIGNED;
    assert_eq!(until(&mut sb, refusal).await.reason.as_known(), Some(not_assigned));

    let released = agent::LeaseReleased {
        workload: workload.clone(),
        epoch: first.epoch,
        ..Default::default()
    };
    send(&mut sa, Up::LeaseReleased(Box::new(released))).await;
    until(&mut sa, epoch_set(0)).await;
    send(&mut sa, request(&workload, 5)).await;
    let second = until(&mut sa, grant).await;
    assert!(second.epoch > first.epoch, "acting again after a release is a new epoch");
    assert_eq!(h.effects("lease.issue").await.len(), 2, "issues are effects, renewals are not");
}
