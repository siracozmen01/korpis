// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where streams are offloaded (§4 and question 1 of docs/14-streams.md): the first repository
//! of an organization or project takes its streams unless told not to, and no second one may;
//! the project's is taken before the organization's; and a node that ran the workload is given
//! its data key and write access to its stream prefix alone, beside its backups, while a node
//! that never did, or a workload no repository takes the streams of, is given nothing.

use crate::agent::Stream;
use crate::backup_target::{Bucket, serving};
use crate::recipes::project;
use crate::snapshots::running;
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use korpis_proto::korpis::v1 as pb;

fn made(
    key: &str,
    prefix: &str,
    project: Option<String>,
    streams: Option<bool>,
) -> pb::CreateRepositoryRequest {
    pb::CreateRepositoryRequest {
        mutation: mutation(key).into(),
        organization: String::new(),
        project,
        name: key.into(),
        provider: "local".into(),
        prefix: prefix.into(),
        retention: pb::RetentionPolicy { keep_daily: 7, ..Default::default() }.into(),
        streams,
        ..Default::default()
    }
}

async fn create(
    h: &Harness,
    mut r: pb::CreateRepositoryRequest,
) -> Result<pb::Repository, connectrpc::ConnectError> {
    r.organization = h.root_org();
    let made = h.repositories(h.root()).create_repository(r).await?;
    Ok(made.into_owned().repository.into_option().unwrap())
}

/// Asks to offload `workload`'s stream; the outcome.
async fn offload(s: &mut Stream, request: &str, workload: &str) -> Outcome {
    let up = agent::JobAccessRequest {
        request: request.into(),
        offload: workload.into(),
        ..Default::default()
    };
    s.send(agent::AgentMessage {
        message: Some(Up::JobAccessRequest(Box::new(up))),
        ..Default::default()
    })
    .await
    .unwrap();
    let pick = |d: &Down| match d {
        Down::JobAccess(a) if a.request == request => a.outcome.clone(),
        _ => None,
    };
    crate::journal::until(s, pick).await
}

#[tokio::test]
async fn the_first_repository_takes_streams_and_a_second_may_not() {
    let h = Harness::new().await;
    let first = create(&h, made("one", "a/", None, None)).await.unwrap();
    assert!(first.streams, "the first of the organization takes its streams");
    let second = create(&h, made("two", "b/", None, None)).await.unwrap();
    assert!(!second.streams, "a second does not, unless told");
    let e = create(&h, made("three", "c/", None, Some(true))).await.unwrap_err();
    assert_eq!(reason(&e), "repository.streams_taken");
    let p = project(&h, &h.root_org(), "games").await;
    let own = create(&h, made("four", "d/", Some(p), None)).await.unwrap();
    assert!(own.streams, "a project may have its own");
    let e = create(&h, made("five", "a/streams/", None, Some(false))).await.unwrap_err();
    assert_eq!(reason(&e), "repository.prefix_overlaps", "nor sit in another's streams");
}

#[tokio::test]
async fn a_node_that_ran_the_workload_is_given_its_stream_prefix_and_no_other_node_anything() {
    let (h, mut s, workload, _, _) = running(Vec::new()).await;
    serving(&h, Bucket::default()).await;
    let Outcome::Refused(e) = offload(&mut s, "o1", &workload).await else { panic!() };
    assert_eq!((e.reason.as_str(), e.retryable), ("offload.none", true), "no repository yet");

    let org_wide = create(&h, made("nightly", "korpis/", None, None)).await.unwrap();
    assert!(org_wide.streams);
    let Outcome::Grant(g) = offload(&mut s, "o2", &workload).await else { panic!() };
    let bare = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, &workload).unwrap();
    let store = g.store.as_option().unwrap();
    assert_eq!(store.key_prefix, format!("repo/korpis/streams/{}/", bare.uuid()));
    assert_eq!(g.data_key.len(), 32);
    let Outcome::Grant(again) = offload(&mut s, "o3", &workload).await else { panic!() };
    assert_eq!(again.data_key, g.data_key, "the workload's one key in the repository");

    let ask = pb::GetWorkloadRequest { id: workload.clone(), ..Default::default() };
    let status = h.workloads(h.root()).get_workload(ask).await.unwrap().into_owned();
    let project = status.status.into_option().unwrap().workload.into_option().unwrap().project;
    create(&h, made("own", "games/", Some(project), None)).await.unwrap();
    let Outcome::Grant(own) = offload(&mut s, "o5", &workload).await else { panic!() };
    let own = own.store.as_option().unwrap().key_prefix.clone();
    assert_eq!(own, format!("repo/games/streams/{}/", bare.uuid()), "the project's first");

    let other = crate::agent::TestAgent::enroll(&h, "fra-7", 2).await;
    let hello = other.hello_at(std::time::SystemTime::now(), &[2; 16]);
    let (mut s2, _) = crate::agent::TestAgent::open(&h, hello).await;
    let Outcome::Refused(e) = offload(&mut s2, "o4", &workload).await else { panic!() };
    assert_eq!((e.reason.as_str(), e.retryable), ("job.not_assigned", false));
}
