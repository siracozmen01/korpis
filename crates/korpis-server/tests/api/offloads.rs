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

pub(crate) fn made(
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

pub(crate) async fn create(
    h: &Harness,
    mut r: pb::CreateRepositoryRequest,
) -> Result<pb::Repository, connectrpc::ConnectError> {
    r.organization = h.root_org();
    let made = h.repositories(h.root()).create_repository(r).await?;
    Ok(made.into_owned().repository.into_option().unwrap())
}

/// Asks to offload `workload`'s stream; the outcome.
pub(crate) async fn offload(s: &mut Stream, request: &str, workload: &str) -> Outcome {
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

/// A sealed segment of `lines` from offset `first`, as a node seals it: its bytes and what the
/// index is told of it.
pub(crate) fn sealed(
    dir: &std::path::Path,
    first: u64,
    lines: &[&str],
) -> (Vec<u8>, pb::StreamSegment) {
    use korpis_stream::record::{Kind, Record, Source};
    let path = dir.join(format!("{first}.kseg"));
    let mut w = korpis_stream::segment::SegmentWriter::create(&path, first).unwrap();
    for (i, line) in lines.iter().enumerate() {
        let offset = first + u64::try_from(i).unwrap();
        let body = line.as_bytes().to_vec();
        let r = Record { offset, time_nanos: 1, source: Source::Stdout, kind: Kind::Text, body };
        w.append(&r).unwrap();
    }
    let s = w.seal().unwrap();
    let segment = pb::StreamSegment {
        first_offset: s.first_offset,
        end_offset: s.end_offset,
        bytes: s.bytes,
        digest: s.digest,
        ..Default::default()
    };
    (std::fs::read(&path).unwrap(), segment)
}

/// The journal record of `event` to `segment` of `workload`'s console.
pub(crate) fn journaled(
    seq: u64,
    workload: &str,
    event: agent::SegmentEvent,
    segment: &pb::StreamSegment,
) -> agent::JournalRecord {
    use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
    let e = agent::StreamSegmentEvent {
        workload: workload.to_owned(),
        kind: pb::StreamKind::STREAM_KIND_CONSOLE.into(),
        event: event.into(),
        segment: segment.clone().into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::StreamSegment(Box::new(e))),
        count: 1,
        ..Default::default()
    }
}

/// Reads `workload`'s offloaded console from `from`, at most `limit` records: the chunks.
pub(crate) async fn read(
    h: &Harness,
    workload: &str,
    (from, limit): (u64, u64),
) -> Result<Vec<pb::ConsoleChunk>, connectrpc::ConnectError> {
    let r = pb::ReadOffloadedRequest {
        workload: workload.into(),
        from_offset: from,
        limit,
        ..Default::default()
    };
    let mut stream = h.streams(h.root()).read_offloaded(r).await?;
    let mut all = Vec::new();
    while let Some(chunk) = stream.message().await? {
        all.push(chunk.to_owned_message());
    }
    Ok(all)
}

pub(crate) fn bodies(chunks: &[pb::ConsoleChunk]) -> Vec<String> {
    let records = chunks.iter().flat_map(|c| c.records.iter());
    records.map(|r| String::from_utf8_lossy(&r.body).into_owned()).collect()
}

#[tokio::test]
async fn an_offloaded_stream_is_read_back_through_the_control_plane_only_as_it_was_sealed() {
    use agent::SegmentEvent as E;
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let Some(minio) = korpis_backup::testing::Minio::start(scratch.path()).await.unwrap() else {
        return;
    };
    minio.bucket("backups").await.unwrap();
    let (h, mut s, workload, _, seq) = running(Vec::new()).await;
    crate::backup_target::serving_with(&h, crate::prunes::real(&minio)).await;
    create(&h, made("nightly", "korpis/", None, None)).await.unwrap();
    let Outcome::Grant(g) = offload(&mut s, "o1", &workload).await else { panic!() };
    let repo = crate::prunes::repo_of(&g);
    let (first, mut one) = sealed(scratch.path(), 0, &["Starting server", "Done"]);
    let (second, mut two) = sealed(scratch.path(), 4, &["Player Alex joined", "Saved"]);
    for (bytes, segment) in [(first, &one), (second.clone(), &two)] {
        let at = (segment.first_offset, segment.digest.as_str());
        korpis_backup::offload::put_segment(&repo, at, bytes).await.unwrap();
    }
    // The node names a repository it was given for the first; for the second, none it was.
    one.offloaded = true;
    one.repository.clone_from(&g.repository);
    two.offloaded = true;
    two.repository = h.root_org().replace("org_", "rep_");
    let records = vec![
        journaled(seq, &workload, E::SEGMENT_EVENT_SEALED, &one),
        journaled(seq + 1, &workload, E::SEGMENT_EVENT_OFFLOADED, &one),
        journaled(seq + 2, &workload, E::SEGMENT_EVENT_SEALED, &two),
        journaled(seq + 3, &workload, E::SEGMENT_EVENT_OFFLOADED, &two),
    ];
    crate::journal::send(&mut s, records).await;
    crate::journal::until(&mut s, |d| crate::journal::ack(d).filter(|a| *a >= seq + 3)).await;

    let chunks = read(&h, &workload, (0, 0)).await.unwrap();
    assert_eq!(bodies(&chunks), ["Starting server", "Done"], "the second is not believed");
    assert_eq!(chunks.last().map(|c| (c.next_offset, c.caught_up)), Some((2, true)));
    let from_one = read(&h, &workload, (1, 1)).await.unwrap();
    assert_eq!(bodies(&from_one), ["Done"]);

    // Replaced by the node with another segment, sealed under the same key: refused, not read.
    let at = korpis_backup::offload::segment_id(0, &one.digest).unwrap();
    repo.put(korpis_backup::seal::Kind::Segment, &at, second).await.unwrap();
    let e = read(&h, &workload, (0, 0)).await.unwrap_err();
    assert_eq!(reason(&e), "stream.segment_altered");
    // Or moved there in the store from another name: it does not open.
    let theirs =
        format!("segments/{}", korpis_backup::offload::segment_id(4, &two.digest).unwrap());
    let store = repo.store();
    let moved = korpis_backup::store::ObjectStore::get(&**store, &theirs).await.unwrap().unwrap();
    korpis_backup::store::ObjectStore::put(&**store, &format!("segments/{at}"), moved)
        .await
        .unwrap();
    let e = read(&h, &workload, (0, 0)).await.unwrap_err();
    assert_eq!(reason(&e), "stream.segment_altered");
}
