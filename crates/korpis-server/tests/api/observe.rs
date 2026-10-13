// SPDX-License-Identifier: AGPL-3.0-or-later

//! Observations and convergence (§4.3 of docs/02-architecture.md): measured field by field
//! against the assignment, and `unknown` once the node is not heard, never a remembered value.
//! Each change of convergence or health is an event (§5 of docs/15-observability.md), the
//! silence included, which no write announces.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{ack, assignment, complete, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use buffa_types::google::protobuf::Timestamp;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> Timestamp {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    Timestamp { seconds: i64::try_from(since.as_secs()).unwrap(), ..Default::default() }
}

struct Seen<'a> {
    workload: &'a str,
    generation: u64,
    intent: &'a str,
    state: pb::ObservedState,
    health: pb::HealthState,
}

async fn report(s: &mut Stream, seen: &Seen<'_>) {
    let o = pb::Observation {
        workload: seen.workload.to_owned(),
        observed_at: now().into(),
        assignment_seen: seen.generation,
        intent_seen: seen.intent.to_owned(),
        power_seen: pb::PowerSeen { version: 1, cycle: 0, ..Default::default() }.into(),
        state: seen.state.into(),
        health: seen.health.into(),
        ..Default::default()
    };
    let observation =
        agent::AgentMessage { message: Some(Up::Observation(Box::new(o))), ..Default::default() };
    s.send(observation).await.unwrap();
    s.send(TestAgent::heartbeat()).await.unwrap();
}

type Changes = Vec<(String, String, String)>;

/// The condition events of `kind` about workload `w`, as (from, to, severity), once the last
/// one changed to `to`: they are decided in the background, a change only the clock makes up to
/// one resync late.
async fn conditions(h: &Harness, w: &str, kind: &str, to: &str) -> Changes {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, w).unwrap().uuid();
    let store = h.db.store().await.unwrap();
    let mut found: Changes = Vec::new();
    for _ in 0..100 {
        found = sqlx::query_as(
            "SELECT detail->>'from', detail->>'to', severity FROM event
              WHERE workload_id = $1 AND kind = $2 ORDER BY seq",
        )
        .bind(id)
        .bind(kind)
        .fetch_all(store.pool())
        .await
        .unwrap();
        if found.last().is_some_and(|(_, last, _): &(String, String, String)| last == to) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no {kind} to {to}: {found:?}");
}

/// The workload's convergence and observed state, once `want` is reached or after a few tries.
async fn status(h: &Harness, w: &str, want: pb::Convergence) -> pb::WorkloadStatus {
    let mut last = None;
    for _ in 0..40 {
        let got = h
            .workloads(h.root())
            .get_workload(pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() })
            .await
            .unwrap()
            .into_owned();
        let status = got.status.into_option().unwrap();
        last = status.convergence.as_known();
        if last == Some(want) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never {want:?}, last {last:?}");
}

#[tokio::test]
async fn convergence_follows_what_the_node_reports_and_is_unknown_when_it_is_not_heard() {
    let h = Harness::with_heartbeat(Duration::from_millis(400)).await;
    // Everything slow happens before the session opens: with heartbeats this short, a node the
    // test is not heartbeating for is unreachable within a second.
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    until(&mut s, ack).await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let w = declared.workload.as_option().unwrap().id.clone();
    let intent = declared.intent.as_option().unwrap().id.clone();
    let created = until(&mut s, assignment(|a| a.plan.is_set())).await;
    status(&h, &w, pb::Convergence::CONVERGENCE_UNKNOWN).await;

    let healthy = pb::HealthState::HEALTH_STATE_HEALTHY;
    let seen = |generation, state| Seen {
        workload: &w,
        generation,
        intent: &intent,
        state,
        health: healthy,
    };
    report(&mut s, &seen(1, pb::ObservedState::OBSERVED_STATE_STARTING)).await;
    status(&h, &w, pb::Convergence::CONVERGENCE_APPLYING).await;
    let plan = created.plan.as_option().unwrap().clone();
    complete(&mut s, &w, &plan, 1).await;
    let applied = until(&mut s, assignment(|a| !a.plan.is_set())).await;
    assert_eq!(applied.generation, 2);
    status(&h, &w, pb::Convergence::CONVERGENCE_APPLYING).await;

    report(&mut s, &seen(2, pb::ObservedState::OBSERVED_STATE_RUNNING)).await;
    let converged = status(&h, &w, pb::Convergence::CONVERGENCE_CONVERGED).await;
    let observation = converged.observation.as_option().unwrap();
    assert_eq!(observation.state.as_known(), Some(pb::ObservedState::OBSERVED_STATE_RUNNING));
    assert_eq!(observation.node, a.node, "the root may see where it runs");
    let sick = Seen {
        health: pb::HealthState::HEALTH_STATE_UNHEALTHY,
        ..seen(2, pb::ObservedState::OBSERVED_STATE_RUNNING)
    };
    report(&mut s, &sick).await;
    let health = conditions(&h, &w, "health.changed", "unhealthy").await;
    assert_eq!(health[0], ("healthy".into(), "unhealthy".into(), "warning".into()));
    report(&mut s, &seen(2, pb::ObservedState::OBSERVED_STATE_CRASHED)).await;
    status(&h, &w, pb::Convergence::CONVERGENCE_FAILING).await;

    drop(s);
    let silent = status(&h, &w, pb::Convergence::CONVERGENCE_UNKNOWN).await;
    let stale = silent.observation.as_option().unwrap().state.as_known();
    assert_eq!(stale, Some(pb::ObservedState::OBSERVED_STATE_UNKNOWN), "never a remembered value");
    let changes = conditions(&h, &w, "convergence.changed", "unknown").await;
    let to: Vec<&str> = changes.iter().map(|(_, to, _)| to.as_str()).collect();
    assert!(to.ends_with(&["converged", "failing", "unknown"]), "{changes:?}");
    let failing = changes.iter().find(|(_, to, _)| to == "failing").unwrap();
    assert_eq!(failing.2, "warning", "a change for the worse");
}
