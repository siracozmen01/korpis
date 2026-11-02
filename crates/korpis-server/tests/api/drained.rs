// SPDX-License-Identifier: AGPL-3.0-or-later

//! A drain on a real control plane. Proposed, it lists every workload on the node and changes
//! nothing; approved, the node drains and the reconciler begins as many migrations as the policy
//! admits, each a plan naming the operation, approved by its approver; paused, it begins nothing
//! more; and a second operation over the node is refused while it has not ended. A workload no
//! other node takes fails alone and pauses the drain, and aborting it leaves the node cordoned.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{ack, assignment, complete, send, until};
use crate::migrating::local;
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held;
use crate::support::{Harness, mutation};
use crate::volumes::{ensured, volume_record};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// `name`, made in `project` on the node whose stream is `s`, its volume reported there.
pub(crate) async fn made(h: &Harness, s: &mut Stream, (project, name): (&str, &str)) -> String {
    let repo = push_paper().await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create(name, project, name, body(&repo, &[])));
    let w = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
    let told = until(s, assignment(|t| t.workload == w && t.plan.is_set())).await;
    let last = complete(s, &w, told.plan.as_option().unwrap(), 1).await;
    held(s, &w).await;
    send(s, vec![volume_record(last + 1, ensured(&w, "data", true, false))]).await;
    assert_eq!(until(s, ack).await, last + 1);
    w
}

/// Proposes an operation of `kind` over `node`, under `policy`.
pub(crate) async fn proposed(
    h: &Harness,
    (kind, node): (pb::OperationKind, &str),
    policy: pb::OperationPolicy,
) -> Result<pb::Operation, connectrpc::ConnectError> {
    let request = pb::ProposeOperationRequest {
        mutation: mutation(&format!("propose-{}", uuid::Uuid::now_v7())).into(),
        kind: kind.into(),
        node: node.to_owned(),
        policy: policy.into(),
        ..Default::default()
    };
    let answer = h.operations(h.root()).propose_operation(request).await?;
    Ok(answer.into_owned().operation.into_option().unwrap())
}

/// Approves `operation`, under `parent` if named.
pub(crate) async fn approved(h: &Harness, operation: &str, parent: &str) -> pb::Operation {
    let request = pb::ApproveOperationRequest {
        mutation: mutation(&format!("approve-{operation}")).into(),
        operation: operation.to_owned(),
        parent: parent.to_owned(),
        ..Default::default()
    };
    let answer = h.operations(h.root()).approve_operation(request).await.unwrap();
    answer.into_owned().operation.into_option().unwrap()
}

/// `operation` as it stands, after one pass of the reconciler.
pub(crate) async fn tended(h: &Harness, operation: &str) -> pb::Operation {
    h.api.app().tend_operations().await.unwrap();
    let request = pb::GetOperationRequest { id: operation.to_owned(), ..Default::default() };
    let answer = h.operations(h.root()).get_operation(request).await.unwrap();
    answer.into_owned().operation.into_option().unwrap()
}

fn states(op: &pb::Operation) -> Vec<pb::OperationItemState> {
    op.items.iter().filter_map(|i| i.state.as_known()).collect()
}

async fn node_status(h: &Harness, node: &str) -> Option<pb::NodeStatus> {
    let request = pb::GetNodeRequest { id: node.to_owned(), ..Default::default() };
    let answer = h.nodes(h.root()).get_node(request).await.unwrap().into_owned();
    answer.node.into_option().unwrap().status.as_known()
}

/// Two nodes, each with its stream, and two workloads made on the first.
async fn two_on_one(h: &Harness) -> (String, Vec<String>, (Stream, Stream)) {
    let (a, b) = (TestAgent::enroll(h, "fra-1", 1).await, TestAgent::enroll(h, "fra-2", 2).await);
    let (mut sa, first) =
        TestAgent::open(h, a.hello_with(SystemTime::now(), &[1; 16], local())).await;
    welcome(&first.unwrap());
    let project = project(h, &h.root_org(), "games").await;
    let db = made(h, &mut sa, (&project, "db")).await;
    let web = made(h, &mut sa, (&project, "web")).await;
    let (sb, first) = TestAgent::open(h, b.hello_with(SystemTime::now(), &[2; 16], local())).await;
    welcome(&first.unwrap());
    (a.node, vec![db, web], (sa, sb))
}

#[tokio::test]
async fn a_drain_migrates_its_node_s_workloads_as_its_policy_admits() {
    use pb::OperationItemState::{
        OPERATION_ITEM_STATE_APPLYING as APPLYING, OPERATION_ITEM_STATE_WAITING as WAITING,
    };
    let h = Harness::new().await;
    let (node, workloads, _streams) = Box::pin(two_on_one(&h)).await;
    let drain = (pb::OperationKind::OPERATION_KIND_DRAIN, node.as_str());
    let op = proposed(&h, drain, pb::OperationPolicy::default()).await.unwrap();
    let subjects: Vec<&str> = op.items.iter().map(|i| i.subject.as_str()).collect();
    assert_eq!(subjects.len(), 2, "every workload on the node: {subjects:?}");
    assert!(workloads.iter().all(|w| subjects.contains(&w.as_str())));
    assert_eq!(
        node_status(&h, &node).await,
        Some(pb::NodeStatus::NODE_STATUS_READY),
        "nothing yet"
    );

    approved(&h, &op.id, "").await;
    assert_eq!(node_status(&h, &node).await, Some(pb::NodeStatus::NODE_STATUS_DRAINING));
    let running = tended(&h, &op.id).await;
    assert_eq!(states(&running), [APPLYING, WAITING], "one at a time");
    let first = &running.items[0];
    let plan = pb::GetPlanRequest { id: first.plan.clone().unwrap(), ..Default::default() };
    let plan = h.plans(h.root()).get_plan(plan).await.unwrap().into_owned().plan.into_option();
    let plan = plan.unwrap();
    assert_eq!(plan.kind.as_known(), Some(pb::PlanKind::PLAN_KIND_MIGRATE));
    assert_eq!(plan.operation.as_deref(), Some(op.id.as_str()), "it names its operation");
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_APPLYING), "approved");

    let again = proposed(&h, drain, pb::OperationPolicy::default()).await.unwrap_err();
    assert_eq!(again.code, ErrorCode::Aborted, "one operation over a node at a time");
    let pause = pb::PauseOperationRequest {
        mutation: mutation("pause").into(),
        operation: op.id.clone(),
        ..Default::default()
    };
    h.operations(h.root()).pause_operation(pause).await.unwrap();
    let paused = tended(&h, &op.id).await;
    assert_eq!(paused.status.as_known(), Some(pb::OperationStatus::OPERATION_STATUS_PAUSED));
    assert_eq!(states(&paused), [APPLYING, WAITING], "a paused drain begins nothing");
}

#[tokio::test]
async fn a_workload_no_other_node_takes_fails_alone_and_pauses_the_drain() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-1", 1).await;
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local())).await;
    welcome(&first.unwrap());
    let project = project(&h, &h.root_org(), "games").await;
    made(&h, &mut sa, (&project, "db")).await;
    let drain = (pb::OperationKind::OPERATION_KIND_DRAIN, a.node.as_str());
    let op = proposed(&h, drain, pb::OperationPolicy::default()).await.unwrap();
    approved(&h, &op.id, "").await;

    let paused = tended(&h, &op.id).await;
    assert_eq!(paused.status.as_known(), Some(pb::OperationStatus::OPERATION_STATUS_PAUSED));
    assert_eq!(states(&paused), [pb::OperationItemState::OPERATION_ITEM_STATE_FAILED]);
    assert!(!paused.items[0].reason.is_empty(), "it says why");
    assert_eq!(paused.reason, paused.items[0].reason, "the pause names the failure");
    let abort = pb::AbortOperationRequest {
        mutation: mutation("abort").into(),
        operation: op.id.clone(),
        reason: "no room".into(),
        ..Default::default()
    };
    let ended = h.operations(h.root()).abort_operation(abort).await.unwrap().into_owned();
    let ended = ended.operation.into_option().unwrap();
    assert_eq!(ended.status.as_known(), Some(pb::OperationStatus::OPERATION_STATUS_ABORTED));
    assert_eq!(node_status(&h, &a.node).await, Some(pb::NodeStatus::NODE_STATUS_CORDONED));
}
