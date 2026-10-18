// SPDX-License-Identifier: AGPL-3.0-or-later

//! A run's console is a stream of its own (§4 of docs/14-streams.md): the segments a node
//! reports for a run it started are indexed under it, with its own offsets from zero, and read by
//! naming the run; a task has no console of no run, and a run is read only through its own
//! workload.

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
    // A run's segment is believed once the node reports starting it, as the run's own reports.
    let started = crate::run_reports::reported(3, &workload, "run.started", &[("run", &run)]);
    send(&mut s, vec![sealed(1, (0, 50)), sealed(2, (0, 50)), started]).await;
    assert_eq!(until(&mut s, ack).await, 3);
    assert_eq!(h.effects("journal.rejected").await.len(), 2, "not yet a run it started");
    send(&mut s, vec![sealed(4, (0, 100)), sealed(5, (100, 180))]).await;
    assert_eq!(until(&mut s, ack).await, 5);
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

#[tokio::test]
async fn a_run_retention_deletes_ends_its_stream_so_its_offload_expires() {
    use agent::SegmentEvent as E;
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let jobs = project(&h, &h.root_org(), "jobs").await;
    let kept_none = pb::RunPolicy { keep_successful: Some(0), ..Default::default() };
    let task = pb::IntentBody {
        lifecycle: pb::Lifecycle::LIFECYCLE_TASK.into(),
        runs: kept_none.into(),
        ..body(&repo, &[])
    };
    let declared = h.workloads(h.root()).declare_intent(create("t", &jobs, "build", task)).await;
    let workload = declared.unwrap().into_owned().workload.into_option().unwrap().id;
    until(&mut s, assignment(|a| a.plan.is_set())).await;
    let run = crate::runs::made(&h, &workload).await.remove(0).id;
    // Held throughout: a run whose node gives the lease back is lost, and kept as failed.
    crate::streams::held(&mut s, &workload).await;
    let started = crate::run_reports::reported(1, &workload, "run.started", &[("run", &run)]);
    let sealed = of_run(segment_record(2, &workload, E::SEGMENT_EVENT_SEALED, (0, 10)), &run);
    let ended = [("run", run.as_str()), ("cause", "exit"), ("exit_status", "0")];
    let ended = crate::run_reports::reported(3, &workload, "run.ended", &ended);
    send(&mut s, vec![started, sealed, ended]).await;
    assert_eq!(until(&mut s, ack).await, 3);

    // Succeeded and kept by none, it is pruned within a pass, and its stream ends with it.
    let store = h.db.store().await.unwrap();
    let ended = "SELECT count(*) FROM stream WHERE run_id IS NOT NULL AND deleted_at IS NOT NULL";
    let mut count = 0;
    for _ in 0..150 {
        count = sqlx::query_scalar::<_, i64>(ended).fetch_one(store.pool()).await.unwrap();
        if count == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert_eq!(count, 1, "the pruned run's stream is ended");
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM run").fetch_one(store.pool()).await.unwrap();
    assert_eq!(left, 0, "the run is pruned");
}
