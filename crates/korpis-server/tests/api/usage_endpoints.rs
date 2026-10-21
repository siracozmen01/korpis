// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bytes by endpoint, and egress (§2 of docs/15-observability.md): what a node's host table
//! counted of a workload's link, sampled with its other counters, recorded as each interval's
//! difference, and read with the record; a sample naming an endpoint twice is not believed.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::metering::sample_at;
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_core::metering::{DAY, day_of};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

const HOUR: Duration = Duration::from_secs(3600);

/// `record`, its counters carrying `egress` and `endpoints`, each a name and its bytes received.
fn counting(
    mut record: agent::JournalRecord,
    egress: u64,
    endpoints: &[(&str, u64)],
) -> agent::JournalRecord {
    if let Some(B::Metering(s)) = record.body.as_mut()
        && let Some(c) = s.counters.as_option_mut()
    {
        c.egress_bytes = Some(egress);
        c.endpoints = endpoints
            .iter()
            .map(|(name, received)| agent::EndpointCounters {
                endpoint: (*name).into(),
                received_bytes: *received,
                received_packets: received / 100,
                sent_bytes: received * 3,
                sent_packets: received / 50,
                ..Default::default()
            })
            .collect();
    }
    record
}

#[tokio::test]
async fn an_endpoints_bytes_and_the_egress_are_recorded_as_each_intervals_difference() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-14", 9).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[9; 16])).await;
    welcome(&first.unwrap());
    assert_eq!(until(&mut s, ack).await, 0);
    let games = project(&h, &h.root_org(), "games").await;
    let declared = create("w", &games, "mc", body(&push_paper().await, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    until(&mut s, assignment(|a| a.workload == workload)).await;

    let start = day_of(SystemTime::now()) - DAY;
    let samples = vec![
        counting(sample_at(1, &workload, start, 100), 9_000, &[("game", 1_200)]),
        counting(sample_at(2, &workload, start + HOUR, 250), 12_000, &[("game", 2_000)]),
        counting(sample_at(3, &workload, start + HOUR * 2, 300), 1, &[("game", 1), ("game", 2)]),
    ];
    send(&mut s, samples).await;
    assert_eq!(until(&mut s, ack).await, 3);
    assert_eq!(h.effects("journal.rejected").await.len(), 1, "an endpoint named twice");

    let asked = pb::ListUsageRequest {
        workload: Some(workload.clone()),
        from: korpis_wire::timestamp(start).into(),
        ..Default::default()
    };
    let read = h.metering(h.root()).list_usage(asked).await.unwrap().into_owned();
    let seen: Vec<_> = read
        .records
        .iter()
        .map(|r| {
            let game = r.endpoints.iter().find(|e| e.endpoint == "game");
            (r.egress_bytes, game.map(|e| (e.received_bytes, e.sent_bytes, e.received_packets)))
        })
        .collect();
    assert_eq!(
        seen,
        [(Some(9_000), Some((1_200, 3_600, 12))), (Some(3_000), Some((800, 2_400, 8)))],
        "the first from zero, the second its difference"
    );
}
