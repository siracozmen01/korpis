// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering samples, as the control plane ingests them (§2 of docs/15-observability.md): the
//! interval it tells every node, a sample measured from the one before it on that node, samples
//! lost leaving one record spanning them, a sample not believed kept as a rejection, gaps where
//! nothing was heard and their supersession, and a closed day's adjustments.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_app::ports::MeteringRead as _;
use korpis_core::id::{Id, IdKind};
use korpis_core::metering::{DAY, RecordKind, day_of, gaps, interval_of};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

const HOUR: Duration = Duration::from_secs(3600);

fn hour(n: u32) -> SystemTime {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(now / 3600 * 3600) - HOUR * 10 + HOUR * n
}

fn sample(seq: u64, workload: &str, at: u32, cpu: u64) -> agent::JournalRecord {
    sample_at(seq, workload, hour(at), cpu)
}

/// A sample of `workload`'s CPU time, `cpu`, for the hour from `start`, as journal record `seq`.
pub fn sample_at(seq: u64, workload: &str, start: SystemTime, cpu: u64) -> agent::JournalRecord {
    let s = agent::MeteringSample {
        workload: workload.to_owned(),
        interval_start: korpis_wire::timestamp(start).into(),
        interval: korpis_wire::duration(HOUR).into(),
        counters: agent::MeterCounters { cpu_usec: Some(cpu), ..Default::default() }.into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::Metering(Box::new(s))),
        count: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_sample_is_measured_from_the_one_before_it_and_lost_ones_leave_a_span() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-6", 3).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[3; 16])).await;
    let told = welcome(&first.unwrap()).metering_interval.into_option();
    assert_eq!(told.map(|d| d.seconds), Some(3600), "every node is told the interval");
    assert_eq!(until(&mut s, ack).await, 0);
    let repo = push_paper().await;
    let games = project(&h, &h.root_org(), "games").await;
    let declared = create("w", &games, "mc", body(&repo, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    until(&mut s, assignment(|a| a.workload == workload)).await;

    let stranger = "wl_00000000000000000000000000000000";
    send(&mut s, vec![sample(1, stranger, 0, 5)]).await;
    assert_eq!(until(&mut s, ack).await, 1);
    assert_eq!(h.effects("journal.rejected").await.len(), 1, "a workload it never held");

    send(&mut s, vec![sample(2, &workload, 0, 100)]).await;
    assert_eq!(until(&mut s, ack).await, 2);
    send(&mut s, vec![sample(2, &workload, 0, 100), sample(3, &workload, 3, 400)]).await;
    assert_eq!(until(&mut s, ack).await, 3, "a duplicate is dropped by the cursor");
    send(&mut s, vec![sample(4, &workload, 1, 450)]).await;
    assert_eq!(until(&mut s, ack).await, 4);
    assert_eq!(h.effects("journal.rejected").await.len(), 2, "a sample older than the last");

    // A counter no store could hold, or a time far from the clock, rejects its record and stops
    // nothing behind it.
    send(&mut s, vec![sample(5, &workload, 5, u64::MAX), sample(6, &workload, 30, 500)]).await;
    assert_eq!(until(&mut s, ack).await, 6, "the journal moves past both");
    assert_eq!(h.effects("journal.rejected").await.len(), 4, "a counter beyond i64, the future");

    let id = Id::parse(IdKind::Workload, &workload).unwrap().uuid();
    let store = h.db.store().await.unwrap();
    let records = store.metering_records(id, hour(0) - HOUR, hour(20)).await.unwrap();
    let seen: Vec<_> =
        records.iter().map(|r| (r.from, r.to, r.used.cpu_usec, r.cause.clone())).collect();
    assert_eq!(
        seen,
        [
            (hour(0), hour(1), Some(100), None),
            (hour(1), hour(4), Some(300), Some("samples lost".to_owned())),
        ],
        "the first from zero, then one span: measured, not divided"
    );
    assert!(records.iter().all(|r| r.kind == RecordKind::Measured && r.node.is_some()));
    assert!(h.effects("journal.unsupported").await.is_empty(), "a sample is no effect");
}

/// A test agent holding a declared workload: its session and the workload.
async fn holding(h: &Harness, name: &str, n: u8) -> (crate::agent::Stream, String) {
    let a = TestAgent::enroll(h, name, n).await;
    let (mut s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[n; 16])).await;
    welcome(&first.unwrap());
    assert_eq!(until(&mut s, ack).await, 0);
    let repo = push_paper().await;
    let games = project(h, &h.root_org(), "games").await;
    let declared = create("w", &games, "mc", body(&repo, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    until(&mut s, assignment(|a| a.workload == workload)).await;
    (s, workload)
}

#[tokio::test]
async fn silence_is_a_gap_until_measured_and_a_closed_day_is_adjusted() {
    let h = Harness::new().await;
    let (mut s, workload) = holding(&h, "fra-7", 4).await;
    let start = day_of(SystemTime::now()) - DAY * 3;
    send(&mut s, vec![sample_at(1, &workload, start, 10)]).await;
    assert_eq!(until(&mut s, ack).await, 1);

    // Nothing heard since: a gap an interval, to two intervals ago; and every day before
    // yesterday closes, its lateness window of a day passed.
    let now = SystemTime::now();
    let cutoff = interval_of(now, HOUR) - HOUR * 2;
    let due = gaps(start + HOUR, cutoff, HOUR).len();
    let kept = h.api.app().keep_metering().await.unwrap();
    assert_eq!((kept.gaps, kept.closed), (due, 2), "the days three and two back close");
    let again = h.api.app().keep_metering().await.unwrap();
    assert!(again.gaps <= 1 && again.closed == 0, "only an hour that ended since: {again:?}");

    // A sample arrives late: one record spanning what was lost, superseding the gaps it covers,
    // an adjustment of the closed day it belongs to.
    send(&mut s, vec![sample_at(2, &workload, start + HOUR * 5, 50)]).await;
    assert_eq!(until(&mut s, ack).await, 2);
    let id = Id::parse(IdKind::Workload, &workload).unwrap().uuid();
    let store = h.db.store().await.unwrap();
    let records = store.metering_records(id, start, start + DAY).await.unwrap();
    let late = records.iter().find(|r| r.used.cpu_usec == Some(40)).expect("the late sample");
    assert_eq!((late.from, late.to), (start + HOUR, start + HOUR * 6));
    assert_eq!(late.kind, RecordKind::Adjustment, "its day closed before it arrived");
    assert_eq!(late.supersedes.len(), 5, "the gaps of the five hours it covers");
    let gap_ids: Vec<_> =
        records.iter().filter(|r| r.kind == RecordKind::Gap).map(|r| r.id).collect();
    assert!(late.supersedes.iter().all(|g| gap_ids.contains(g)));
    assert_eq!(records[0].kind, RecordKind::Measured, "the first, before its day closed");

    // Read as an invoice sums it, the closed day is what it was when it closed, its gaps still
    // in it, and the adjustment is in the day it was written into: each use counted once.
    let read = |from: SystemTime, by_period| pb::ListUsageRequest {
        workload: Some(workload.clone()),
        from: korpis_wire::timestamp(from).into(),
        to: korpis_wire::timestamp(from + DAY).into(),
        by_period,
        ..Default::default()
    };
    let metering = h.metering(h.root());
    let kinds = |r: &pb::ListUsageResponse| -> Vec<pb::UsageKind> {
        r.records.iter().filter_map(|r| r.kind.as_known()).collect()
    };
    let in_day = gaps(start + HOUR, start + DAY, HOUR).len();
    let closed = metering.list_usage(read(start, true)).await.unwrap().into_owned();
    assert!(!kinds(&closed).contains(&pb::UsageKind::USAGE_KIND_ADJUSTMENT), "{closed:?}");
    assert_eq!(
        kinds(&closed).iter().filter(|k| **k == pb::UsageKind::USAGE_KIND_GAP).count(),
        in_day
    );
    let today = metering.list_usage(read(day_of(now), true)).await.unwrap().into_owned();
    let adjusted: Vec<_> = today.records.iter().filter(|r| r.cpu_usec == Some(40)).collect();
    assert_eq!(adjusted.len(), 1, "the adjustment, in the day it was recorded");
    // By its intervals, what was used: the adjustment in place of the gaps it supersedes.
    let used = metering.list_usage(read(start, false)).await.unwrap().into_owned();
    assert!(used.records.iter().any(|r| r.cpu_usec == Some(40)));
    let gaps_left = kinds(&used).iter().filter(|k| **k == pb::UsageKind::USAGE_KIND_GAP).count();
    assert_eq!(gaps_left, in_day - 5, "the five it covers are superseded");
}

#[tokio::test]
async fn a_workload_placed_and_never_sampled_is_a_gap_from_its_placement() {
    let h = Harness::new().await;
    let (mut s, workload) = holding(&h, "fra-8", 5).await;
    let id = Id::parse(IdKind::Workload, &workload).unwrap().uuid();
    let store = h.db.store().await.unwrap();
    let placed = interval_of(SystemTime::now(), HOUR) - HOUR * 6;
    let seconds = placed.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    sqlx::query("UPDATE placement SET bound_at = to_timestamp($2::bigint) WHERE workload_id = $1")
        .bind(id)
        .bind(i64::try_from(seconds).unwrap())
        .execute(store.pool())
        .await
        .unwrap();
    let kept = h.api.app().keep_metering().await.unwrap();
    assert!(kept.gaps >= 4, "silent since it was placed, its node unheard: {kept:?}");

    // Its first sample is measured from its placement, superseding the gaps it covers.
    send(&mut s, vec![sample_at(1, &workload, placed + HOUR * 2, 80)]).await;
    assert_eq!(until(&mut s, ack).await, 1);
    let records = store.metering_records(id, placed, placed + DAY).await.unwrap();
    let first = records.iter().find(|r| r.kind != RecordKind::Gap).unwrap();
    assert_eq!((first.from, first.used.cpu_usec), (placed, Some(80)));
    assert_eq!(first.supersedes.len(), 3, "the three hours from its placement to the sample's end");
}
