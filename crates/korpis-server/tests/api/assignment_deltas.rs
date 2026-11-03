// SPDX-License-Identifier: AGPL-3.0-or-later

//! Assignment deltas: an agent that says it takes them is sent, for a change, only what is new or
//! at a new generation since the set last sent; asked for a resync, the whole set; and, coming back
//! holding the set last sent to it, what changed while it was away.

use crate::agent::{Stream, TestAgent, next};
use crate::assignments::{connect_with, set};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;

fn delta(m: &agent::ControlMessage) -> agent::AssignmentDelta {
    match m.message.as_ref() {
        Some(Down::AssignmentDelta(d)) => (**d).clone(),
        other => panic!("expected a delta, got {other:?}"),
    }
}

/// The workloads of the assignments `d` adds.
fn upserted(d: &agent::AssignmentDelta) -> Vec<String> {
    let decoded = d.upserts.iter().map(|s| agent::Assignment::decode_from_slice(&s.assignment));
    decoded.map(|a| a.unwrap().workload).collect()
}

async fn declared(h: &Harness, repo: &str, project: &str, name: &str) -> String {
    let workloads = h.workloads(h.root());
    let made = workloads.declare_intent(create(name, project, name, body(repo, &[]))).await;
    made.unwrap().into_owned().workload.into_option().unwrap().id
}

async fn resync(stream: &mut Stream, held: &str) {
    let r = agent::Resync { set_digest: held.to_owned(), ..Default::default() };
    let m = agent::AgentMessage { message: Some(Up::Resync(Box::new(r))), ..Default::default() };
    stream.send(m).await.unwrap();
}

#[tokio::test]
async fn an_agent_taking_deltas_is_sent_what_changed_and_the_whole_set_when_it_asks() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let mut stream = connect_with(&h, &a, (1, ""), true).await;
    let empty = set(&next(&mut stream).await);
    assert!(empty.assignments.is_empty(), "nothing it holds is known: the whole set");

    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let first = declared(&h, &repo, &project, "mc").await;
    let added = delta(&next(&mut stream).await);
    assert_eq!(added.base_digest, empty.set_digest, "from the set last sent");
    assert_eq!((upserted(&added), added.removed.len()), (vec![first.clone()], 0));

    resync(&mut stream, &empty.set_digest).await;
    let whole = set(&next(&mut stream).await);
    assert_eq!(whole.set_digest, added.set_digest, "the same set, whole");
    assert_eq!(whole.assignments.len(), 1);
    drop(stream);

    let second = declared(&h, &repo, &project, "mc2").await;
    let mut again = connect_with(&h, &a, (2, &whole.set_digest), true).await;
    let missed = delta(&next(&mut again).await);
    assert_eq!(missed.base_digest, whole.set_digest, "from the set it came back holding");
    assert_eq!(upserted(&missed), vec![second], "only what changed while it was away");
}
