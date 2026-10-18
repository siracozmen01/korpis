// SPDX-License-Identifier: AGPL-3.0-or-later

//! A run's console is a stream of its own (§4 of docs/14-streams.md): the segments a node
//! reports for a run are indexed under it, with its own offsets from zero, and read by naming the
//! run; a task has no console of no run, and a run is read only through its own workload.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::refusal::reason;
use crate::streams::{held_and_released, segment_record};
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// `record` as a segment of `run`'s stream.
fn of_run(mut record: agent::JournalRecord, run: &str) -> agent::JournalRecord {
    if let Some(B::StreamSegment(e)) = record.body.as_mut() {
        run.clone_into(&mut e.run);
    }
    record
}

fn get(workload: &str, run: &str) -> pb::GetStreamRequest {
    pb::GetStreamRequest {
        workload: workload.to_owned(),
        run: run.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_run_s_segments_are_indexed_under_it_and_read_by_naming_it() {
    use agent::SegmentEvent as E;
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let jobs = project(&h, &h.root_org(), "jobs").await;
    let task =
        pb::IntentBody { lifecycle: pb::Lifecycle::LIFECYCLE_TASK.into(), ..body(&repo, &[]) };
    let declared = h.workloads(h.root()).declare_intent(create("t", &jobs, "build", task)).await;
    let workload = declared.unwrap().into_owned().workload.into_option().unwrap().id;
    until(&mut s, assignment(|a| a.plan.is_set())).await;
    let run = crate::runs::made(&h, &workload).await.remove(0).id;
    let streams = h.streams(h.root());
    let before = streams.get_stream(get(&workload, &run)).await.unwrap().into_owned();
    assert!(before.stream.as_option().unwrap().segments.is_empty(), "known before any seal");
    let none = streams.get_stream(get(&workload, "")).await.unwrap_err();
    assert_eq!(none.code, ErrorCode::NotFound, "a task's runs write its console: {none:?}");
    held_and_released(&mut s, &workload).await;

    let sealed =
        |seq, range| of_run(segment_record(seq, &workload, E::SEGMENT_EVENT_SEALED, range), &run);
    send(&mut s, vec![sealed(1, (0, 100)), sealed(2, (100, 180))]).await;
    assert_eq!(until(&mut s, ack).await, 2);
    let got = streams.get_stream(get(&workload, &run)).await.unwrap().into_owned();
    let indexed = got.stream.as_option().unwrap();
    assert_eq!(indexed.run, run);
    let offsets: Vec<_> = indexed.segments.iter().map(|g| (g.first_offset, g.end_offset)).collect();
    assert_eq!(offsets, [(0, 100), (100, 180)], "its own offsets from zero");
    let still = streams.get_stream(get(&workload, "")).await.unwrap_err();
    assert_eq!(still.code, ErrorCode::NotFound, "nothing went to a console of no run");

    // A run is read through its own workload only.
    let other = create("o", &jobs, "other", body(&repo, &[]));
    let other = h.workloads(h.root()).declare_intent(other).await.unwrap().into_owned();
    let other = other.workload.into_option().unwrap().id;
    let crossed = streams.get_stream(get(&other, &run)).await.unwrap_err();
    assert_eq!(crossed.code, ErrorCode::NotFound, "{crossed:?}");
    let malformed = streams.get_stream(get(&workload, "wl_1")).await.unwrap_err();
    assert_eq!(malformed.code, ErrorCode::InvalidArgument, "{}", reason(&malformed));
}
