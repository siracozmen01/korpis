// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload whose driver takes snapshots the other node's driver restores moves with its state:
//! its plan says quiesce captures it, and both nodes are told so with the state volume it crosses
//! in, in a class on the source's own disk. An approval whose destination no longer restores that
//! state, its driver having said otherwise since the proposal, is refused rather than carried out as
//! a stop the approver never read.

use crate::agent::{TestAgent, welcome};
use crate::journal::{assignment, complete, until};
use crate::lifecycle::approve;
use crate::migrating::{local, moving_from};
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held;
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// `hello` with every driver saying its snapshots restore by Firecracker 1.17.0 on a 6.8 kernel.
fn restoring(mut hello: agent::AgentMessage) -> agent::AgentMessage {
    if let Some(Up::Hello(h)) = &mut hello.message {
        for d in &mut h.drivers {
            let mut caps = d.capabilities.as_option().cloned().unwrap_or_default();
            caps.runtime_snapshot = pb::SnapshotCaps {
                hypervisor_version: "1.17.0".into(),
                kernel_min: "6.8".into(),
                kernel_max: "6.8".into(),
                ..Default::default()
            }
            .into();
            d.capabilities = caps.into();
        }
    }
    hello
}

/// What step `step` of plan `plan` says quiesce does.
async fn quiesce(h: &Harness, plan: &str) -> Option<String> {
    let request = pb::GetPlanRequest { id: plan.to_owned(), ..Default::default() };
    let got = h.plans(h.root()).get_plan(request).await.unwrap().into_owned();
    let step = got.plan.into_option().unwrap().steps.into_iter().next().unwrap();
    step.detail.get("quiesce").and_then(|v| match &v.kind {
        Some(pb::__buffa::oneof::value::Kind::StringValue(s)) => Some(s.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn a_move_between_nodes_restoring_each_other_s_snapshots_captures_the_state() {
    let h = Harness::new().await;
    let m = Box::pin(moving_from(h, |repo| body(repo, &[]), (None, restoring))).await;
    assert_eq!(quiesce(&m.h, &m.plan).await.as_deref(), Some("capture"));
    let role = m.told.migration.as_option().unwrap();
    assert!(role.live, "the destination is told it resumes the state");
    let state = role.volumes.iter().find(|v| v.name == "live_state").expect("its state volume");
    assert_eq!(state.storage_class, "local", "a class on the source's own disk that crosses");
    assert_eq!(state.source_helper_id, format!("{}.live_state", m.db));
}

#[tokio::test]
async fn an_approval_whose_destination_no_longer_restores_the_state_is_refused() {
    let h = Harness::new().await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let hello = |n: &TestAgent, seed| n.hello_with(SystemTime::now(), &[seed; 16], local());
    let (mut sa, first) = TestAgent::open(&h, restoring(hello(&a, 1))).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("db", &project, "db", body(&repo, &[])));
    let db = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
    let told = until(&mut sa, assignment(|t| t.workload == db && t.plan.is_set())).await;
    complete(&mut sa, &db, told.plan.as_option().unwrap(), 1).await;
    held(&mut sa, &db).await;
    let (sb, first) = TestAgent::open(&h, restoring(hello(&b, 2))).await;
    welcome(&first.unwrap());
    let request = pb::MigrateWorkloadRequest {
        mutation: mutation("move").into(),
        workload: db.clone(),
        node: b.node.clone(),
        ..Default::default()
    };
    let proposed = workloads.migrate_workload(request).await.unwrap().into_owned();
    let plan = proposed.plan.into_option().unwrap().id;
    assert_eq!(quiesce(&h, &plan).await.as_deref(), Some("capture"));

    // The destination's driver says, at its next hello, that it restores nothing.
    drop(sb);
    let (_sb, first) = TestAgent::open(&h, hello(&b, 3)).await;
    welcome(&first.unwrap());
    let refused = h.plans(h.root()).approve_plan(approve("go", &plan)).await.unwrap_err();
    assert_eq!(reason(&refused), "migration.unrestorable");
}
