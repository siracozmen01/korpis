// SPDX-License-Identifier: AGPL-3.0-or-later

//! A stream's segment index (§4 of docs/14-streams.md): kept from what the node journals, so the
//! control plane knows where every run of records is and how much is held only on the node, and
//! believed only from a node that once held the workload's lease: a segment is sealed as the
//! container goes and pruned long after, when the node no longer runs it.

use crate::agent::{Stream, TestAgent, next, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_app::ports::StreamRead as _;
use korpis_core::id::{Id, IdKind};
use korpis_core::workload::stream::StreamKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::stream::Retention;
use std::time::SystemTime;

fn segment_record(
    seq: u64,
    workload: &str,
    event: agent::SegmentEvent,
    (first, end): (u64, u64),
) -> agent::JournalRecord {
    let segment = pb::StreamSegment {
        first_offset: first,
        end_offset: end,
        bytes: 4096,
        digest: format!("sha256:{first:064}"),
        ..Default::default()
    };
    let e = agent::StreamSegmentEvent {
        workload: workload.to_owned(),
        kind: pb::StreamKind::STREAM_KIND_CONSOLE.into(),
        event: event.into(),
        segment: segment.into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::StreamSegment(Box::new(e))),
        count: 1,
        ..Default::default()
    }
}

async fn up(s: &mut Stream, message: Up) {
    let m = agent::AgentMessage { message: Some(message), ..Default::default() };
    s.send(m).await.unwrap();
}

/// Takes `workload`'s lease and gives it back, as a node that ran it and was then retired.
pub(crate) async fn held_and_released(s: &mut Stream, workload: &str) {
    let request = agent::LeaseRequest {
        workload: workload.to_owned(),
        nonce: vec![7; 8],
        ..Default::default()
    };
    up(s, Up::LeaseRequest(Box::new(request))).await;
    let epoch = loop {
        if let Some(Down::LeaseGrant(g)) = next(s).await.message {
            break g.epoch;
        }
    };
    let released =
        agent::LeaseReleased { workload: workload.to_owned(), epoch, ..Default::default() };
    up(s, Up::LeaseReleased(Box::new(released))).await;
}

#[tokio::test]
async fn the_index_follows_what_the_node_seals_and_prunes() {
    use agent::SegmentEvent as E;
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
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
    let get = pb::GetStreamRequest { workload: workload.clone(), ..Default::default() };
    let before = h.streams(h.root()).get_stream(get.clone()).await.unwrap().into_owned();
    let unsealed = before.stream.as_option().unwrap();
    assert!(unsealed.id.is_empty() && unsealed.segments.is_empty(), "known before any seal");
    held_and_released(&mut s, &workload).await;

    send(
        &mut s,
        vec![
            segment_record(1, &workload, E::SEGMENT_EVENT_SEALED, (0, 100)),
            segment_record(2, &workload, E::SEGMENT_EVENT_SEALED, (100, 250)),
            segment_record(3, &workload, E::SEGMENT_EVENT_PRUNED, (0, 100)),
            segment_record(
                4,
                "wl_00000000000000000000000000000000",
                E::SEGMENT_EVENT_SEALED,
                (0, 1),
            ),
        ],
    )
    .await;
    assert_eq!(until(&mut s, ack).await, 4);
    let store = h.db.store().await.unwrap();
    let w = Id::parse(IdKind::Workload, &workload).unwrap().uuid();
    let stream = store.stream(w, StreamKind::Console).await.unwrap().expect("indexed");
    assert_eq!(stream.segments.len(), 2);
    assert_eq!(stream.node_only_bytes(), 4096, "the pruned segment is no longer on the node");
    assert_eq!(stream.lost(), [(0, 100)], "and its records, never offloaded, are gone");
    assert_eq!(h.effects("stream.segment.seal").await.len(), 2);
    assert_eq!(h.effects("journal.rejected").await.len(), 1, "a stranger's segment is refused");

    let got = h.streams(h.root()).get_stream(get).await.unwrap().into_owned();
    let indexed = got.stream.as_option().unwrap();
    assert!(indexed.id.starts_with("str_"), "{indexed:?}");
    assert_eq!((indexed.segments.len(), indexed.node_only_bytes), (2, 4096));
    let lost: Vec<_> = got.lost.iter().map(|r| (r.first, r.end)).collect();
    assert_eq!(lost, [(0, 100)], "pruned before it was offloaded");
    assert_eq!(indexed.retention, Some(Retention::RetentionBytes(64 * 1024 * 1024)), "the default");
}
