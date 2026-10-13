// SPDX-License-Identifier: AGPL-3.0-or-later

//! Assignments (§4.8 of docs/02-architecture.md): the set a node holds, signed by the control
//! plane's assignment key, sent when it changes, and not sent again to a node that holds it.

use crate::agent::{Stream, TestAgent, next, welcome};
use crate::lifecycle::{approve, power};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use buffa::Message as _;
use ed25519_dalek::Verifier as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

fn set(m: &agent::ControlMessage) -> agent::AssignmentSet {
    match m.message.as_ref() {
        Some(Down::AssignmentSet(s)) => (**s).clone(),
        other => panic!("expected an assignment set, got {other:?}"),
    }
}

/// Verifies the one assignment of `set` against the agent's keys and decodes it.
fn only(a: &TestAgent, set: &agent::AssignmentSet) -> agent::Assignment {
    let [signed] = set.assignments.as_slice() else { panic!("one assignment, got {set:?}") };
    let key = a.assignment_keys.iter().find(|k| k.key_id == signed.key_id).expect("a known key");
    let public: [u8; 32] = key.public_key.as_slice().try_into().unwrap();
    let verifying = ed25519_dalek::VerifyingKey::from_bytes(&public).unwrap();
    let signature = ed25519_dalek::Signature::from_slice(&signed.signature).unwrap();
    verifying.verify(&signed.assignment, &signature).expect("signed by the control plane");
    agent::Assignment::decode_from_slice(&signed.assignment).unwrap()
}

async fn connect(h: &Harness, a: &TestAgent, n: u8, held: &str) -> Stream {
    let mut hello = a.hello_at(SystemTime::now(), &[n; 16]);
    if let Some(Up::Hello(x)) = hello.message.as_mut() {
        held.clone_into(&mut x.set_digest);
    }
    let (stream, first) = TestAgent::open(h, hello).await;
    welcome(&first.unwrap());
    stream
}

#[tokio::test]
async fn a_node_is_sent_its_signed_set_when_it_changes_and_not_when_it_holds_it() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let mut stream = connect(&h, &a, 1, "").await;
    let empty = set(&next(&mut stream).await);
    assert!(empty.assignments.is_empty(), "nothing is placed here yet");

    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let first = set(&next(&mut stream).await);
    let assignment = only(&a, &first);
    assert_eq!(
        (assignment.workload.as_str(), assignment.node.as_str()),
        (workload.as_str(), a.node.as_str())
    );
    assert_eq!(assignment.generation, 1);
    let intent = declared.intent.as_option().unwrap();
    assert_eq!(assignment.target.as_option().unwrap().id, intent.id, "the approved intent");
    assert!(!assignment.recipe_manifest.is_empty(), "the node needs no registry to know it");
    let plan = assignment.plan.as_option().expect("the applying plan's node steps");
    assert!(!plan.steps.is_empty());
    let ack = agent::Ack { set_digest: first.set_digest.clone(), ..Default::default() };
    stream
        .send(agent::AgentMessage { message: Some(Up::Ack(Box::new(ack))), ..Default::default() })
        .await
        .unwrap();

    let stopped = h.workloads(h.root()).stop(power("s", &workload, 1)).await.unwrap().into_owned();
    let stop_plan = stopped.plan.as_option().unwrap();
    if stop_plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        h.plans(h.root()).approve_plan(approve("a", &stop_plan.id)).await.unwrap();
    }
    let second = set(&next(&mut stream).await);
    let changed = only(&a, &second);
    assert_eq!(changed.generation, 2, "a change is a new generation");
    let state = changed.power.as_option().unwrap().desired.as_known();
    assert_eq!(state, Some(pb::PowerState::POWER_STATE_STOPPED));
    drop(stream);

    let mut again = connect(&h, &a, 2, &second.set_digest).await;
    match next(&mut again).await.message {
        Some(Down::InSync(s)) => assert_eq!(s.set_digest, second.set_digest),
        other => panic!("a node holding its set is told InSync, got {other:?}"),
    }
    let store = h.db.store().await.unwrap();
    let acked: Option<String> =
        sqlx::query_scalar("SELECT acked_set_digest FROM node WHERE acked_set_digest IS NOT NULL")
            .fetch_optional(store.pool())
            .await
            .unwrap()
            .flatten();
    assert_eq!(acked, Some(first.set_digest), "the acknowledgement is recorded");
}
