// SPDX-License-Identifier: AGPL-3.0-or-later

//! A running workload's identity (§9.5 of docs/08-identity.md): its node asks for a credential
//! on its behalf, the control plane issues one only for a workload it places there under a live
//! lease, and the credential authenticates as the workload until the workload leaves the node.

use crate::agent::{Stream, TestAgent, next, welcome};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::workload_credential::Outcome;
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

/// Asks for `workload`'s credential and waits for the answer to that request.
async fn ask(stream: &mut Stream, workload: &str, request: &str) -> Outcome {
    let up = agent::CredentialRequest {
        request: request.to_owned(),
        workload: workload.to_owned(),
        ..Default::default()
    };
    send(stream, Up::CredentialRequest(Box::new(up))).await;
    for _ in 0..20 {
        if let Some(Down::WorkloadCredential(c)) = next(stream).await.message
            && c.request == request
        {
            return c.outcome.clone().unwrap();
        }
    }
    panic!("no answer to {request}");
}

fn refused(o: &Outcome) -> &str {
    match o {
        Outcome::Refused(e) => &e.reason,
        Outcome::Credential(_) => "issued",
    }
}

#[tokio::test]
async fn a_node_obtains_a_credential_only_for_what_it_runs_and_it_acts_as_the_workload() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let mut stream = connect(&h, &a, 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();

    let early = ask(&mut stream, &workload, "r1").await;
    assert_eq!(refused(&early), "credential.no_lease", "placed here, but not yet leased");
    let lease =
        agent::LeaseRequest { workload: workload.clone(), nonce: vec![1; 8], ..Default::default() };
    send(&mut stream, Up::LeaseRequest(Box::new(lease))).await;
    let Outcome::Credential(issued) = ask(&mut stream, &workload, "r2").await else {
        panic!("a leased, placed workload gets its credential");
    };
    assert!(issued.secret.starts_with("korpiswl_"));
    let subject = issued.subject.as_option().and_then(|s| s.kind.clone());
    assert!(
        matches!(subject, Some(pb::__buffa::oneof::subject::Kind::Workload(w)) if w == workload)
    );

    let as_workload = h.workloads(h.config(Some(&issued.secret)));
    let read = pb::GetWorkloadRequest { id: workload.clone(), ..Default::default() };
    let e = as_workload.get_workload(read.clone()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "authenticated as itself, which holds no grant yet");
    let forged = h.workloads(h.config(Some("korpiswl_not-a-credential")));
    let e = forged.get_workload(read.clone()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unauthenticated);

    let elsewhere = TestAgent::enroll(&h, "fra-4", 2).await;
    let mut other = connect(&h, &elsewhere, 2).await;
    let theirs = ask(&mut other, &workload, "r3").await;
    assert_eq!(refused(&theirs), "credential.not_placed_here", "a node vouches only for its own");

    // The workload leaves the node: its credential stops at once, not at its expiry.
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, &workload).unwrap();
    let store = h.db.store().await.unwrap();
    sqlx::query("DELETE FROM placement WHERE workload_id = $1")
        .bind(id.uuid())
        .execute(store.pool())
        .await
        .unwrap();
    let e = as_workload.get_workload(read).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unauthenticated, "a credential is only good where it runs");
}
