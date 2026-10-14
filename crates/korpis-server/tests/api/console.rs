// SPDX-License-Identifier: AGPL-3.0-or-later

//! Console input a node reports (§6 of docs/14-streams.md): recorded as the act of the subject
//! it names only if the grant it names is that subject's and permitted the input. A node can
//! attribute input only to someone who could have given it.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held_and_released;
use crate::support::Harness;
use korpis_core::principal::Principal;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::principal;
use std::time::SystemTime;
use uuid::Uuid;

fn typed(seq: u64, workload: &str, actor: Uuid, grant: &str) -> agent::JournalRecord {
    let bytes = pb::Value { kind: Some(pb::value::Kind::IntValue(5)), ..Default::default() };
    let effect = pb::Effect {
        workload: Some(workload.to_owned()),
        action: "workload.console.write".into(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED.into(),
        actor: principal::to_pb(&Principal::Token(actor)).into(),
        grant: Some(grant.to_owned()),
        after: [("bytes".to_owned(), bytes)].into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::Effect(Box::new(effect))),
        count: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_node_attributes_input_only_to_whoever_could_have_typed_it() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-4", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[2; 16])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    until(&mut s, assignment(|a| a.plan.is_set())).await;
    held_and_released(&mut s, &workload).await;

    let (typist, _) = h.bare_token().await;
    let typing = h.issue_to(typist, "typing", &["workload.console.write"]).await;
    let (reader, _) = h.bare_token().await;
    let reading = h.issue_to(reader, "reading", &["workload.read"]).await;
    send(
        &mut s,
        vec![
            typed(1, &workload, typist, &typing),
            typed(2, &workload, reader, &typing),
            typed(3, &workload, reader, &reading),
        ],
    )
    .await;
    assert_eq!(until(&mut s, ack).await, 3);
    let recorded = h.effects("workload.console.write").await;
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let leaf = korpis_core::id::Id::parse(korpis_core::id::IdKind::Grant, &typing).unwrap().uuid();
    assert_eq!(recorded[0].1, Some(leaf), "under the typist's own grant");
    assert_eq!(
        h.effects("journal.rejected").await.len(),
        2,
        "someone else's grant, and a grant that does not permit typing, are refused"
    );
}
