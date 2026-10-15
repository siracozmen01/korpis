// SPDX-License-Identifier: AGPL-3.0-or-later

//! A driver that refuses after the bind (§2.1 of docs/05-scheduling.md), on real agents: both
//! nodes accept when the shortlist asks, the workload is bound to the preferred one with its plan
//! approved to apply later, and that node's driver refuses by the time the plan reaches it. The
//! agent journals the refusal as `step.refused`, nothing having been made there, and the workload
//! converges on the other node, its plan never failed.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation, push_paper};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Node `name` joined and its agent started over `runtime`; the agent.
async fn node(cp: &ControlPlane, dir: &tempfile::TempDir, name: &str, runtime: &InMemory) -> Agent {
    let (token, ca) = cp.token(name).await;
    let state = StateDir::new(dir.path().join(name));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let host = InMemoryHost::default();
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    agent
}

/// The status of `w` once `pick` holds of it.
async fn status(
    cp: &ControlPlane,
    w: &str,
    pick: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    for _ in 0..300 {
        let s = cp.status(w).await;
        if pick(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w} never reached the status awaited: {:?}", cp.status(w).await);
}

/// The wire id of the node named `name`.
async fn node_id(cp: &ControlPlane, name: &str) -> String {
    let listed = pb::ListNodesRequest { organization: cp.root_org.clone(), ..Default::default() };
    let nodes = cp.nodes().list_nodes(listed).await.unwrap().into_owned().nodes;
    nodes.into_iter().find(|n| n.name == name).unwrap().id
}

/// Declares workload `name` for approval; it and its plan.
async fn proposed(cp: &ControlPlane, project: &str, name: &str) -> (String, String) {
    let recipe =
        pb::RecipeRef { name: push_paper().await, version: "1.21.4".into(), ..Default::default() };
    let declare = pb::DeclareIntentRequest {
        mutation: mutation("declare").into(),
        target: Some(Target::Create(Box::new(pb::NewWorkload {
            project: project.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        }))),
        body: pb::IntentBody { recipe: recipe.into(), ..Default::default() }.into(),
        propose: true,
        ..Default::default()
    };
    let out = cp.workloads().declare_intent(declare).await.unwrap().into_owned();
    (out.workload.as_option().unwrap().id.clone(), out.plan.as_option().unwrap().id.clone())
}

#[tokio::test]
async fn a_driver_that_refuses_after_the_bind_sends_the_workload_and_its_plan_elsewhere() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (InMemory::new(), InMemory::new());
    let _a = node(&cp, &dir, "fra-1", &first).await;
    let _b = node(&cp, &dir, "fra-2", &second).await;
    cp.measured().await;
    let project = cp.project().await;
    let (w, plan) = proposed(&cp, &project, "mc").await;

    let approve = pb::ApprovePlanRequest {
        mutation: mutation("approve").into(),
        plan: plan.clone(),
        apply_after: korpis_server::api::wire::timestamp(
            SystemTime::now() + Duration::from_secs(3),
        )
        .into(),
        ..Default::default()
    };
    cp.client(pb::PlanServiceClient::new).approve_plan(approve).await.unwrap();
    let bound = cp.status(&w).await.placement.into_option().unwrap().node;
    let (one, two) = (node_id(&cp, "fra-1").await, node_id(&cp, "fra-2").await);
    let (refusing, other) = if bound == one { (&first, two) } else { (&second, one) };
    refusing.reject(Some(driver::Rejection {
        reason: pb::UnsatisfiableReason::UNSATISFIABLE_REASON_PREPARE_REJECTED.into(),
        detail: "no such seccomp profile".into(),
        ..Default::default()
    }));

    let converged = |s: &pb::WorkloadStatus| {
        s.convergence.as_known() == Some(pb::Convergence::CONVERGENCE_CONVERGED)
    };
    let s = status(&cp, &w, converged).await;
    let placement = s.placement.as_option().unwrap();
    assert_eq!(placement.node, other, "converged on the node that accepts");
    let why = placement.explanation.as_option().unwrap();
    let left_out = why.filtered.iter().find(|r| r.node == bound).expect("the refusing node");
    assert_eq!(left_out.check, "prepare");
    assert!(left_out.detail.contains("no such seccomp profile"), "{}", left_out.detail);
    assert_eq!(cp.plan(&plan).await.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_APPLIED));
    assert!(refusing.objects().is_empty(), "nothing was made on the refusing node");
    let refused: i64 = cp
        .scalar("SELECT count(*) FROM effect WHERE action = 'step.pull_recipe' AND error->>'reason' = 'step.refused'")
        .await;
    assert_eq!(refused, 1);
}
