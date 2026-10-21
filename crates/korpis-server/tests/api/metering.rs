// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering samples, as the control plane ingests them (§2 of docs/15-observability.md): the
//! interval it tells every node, a sample measured from the one before it on that node, samples
//! lost leaving one record spanning them, and a sample not believed kept as a rejection.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_app::ports::MeteringRead as _;
use korpis_core::id::{Id, IdKind};
use korpis_core::metering::RecordKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use std::time::{Duration, SystemTime};

const HOUR: Duration = Duration::from_secs(3600);

fn hour(n: u32) -> SystemTime {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(now / 3600 * 3600) - HOUR * 10 + HOUR * n
}

fn sample(seq: u64, workload: &str, at: u32, cpu: u64) -> agent::JournalRecord {
    let s = agent::MeteringSample {
        workload: workload.to_owned(),
        interval_start: korpis_wire::timestamp(hour(at)).into(),
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
