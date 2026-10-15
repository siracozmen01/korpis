// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node resolves an install's provider step through the control plane (§4 of
//! docs/09-recipes.md): only where the workload is placed, only through the installation its
//! intent pinned, and a step that resolves to whatever a channel held marks the workload
//! unreproducible, once.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{assignment, until};
use crate::recipes::{body, create, project, push_paper_like};
use crate::step::serving;
use crate::step_pin::with_step;
use crate::support::{Harness, mutation};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::step_resolution::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind;
use std::time::SystemTime;

/// Asks for action `action` of step `step` of `plan` to be resolved; the outcome.
async fn resolve(s: &mut Stream, request: &str, (plan, step, action): (&str, u32, u32)) -> Outcome {
    let up = agent::StepRequest {
        request: request.into(),
        plan: plan.into(),
        step,
        action,
        platform: pb::Platform { os: "linux".into(), arch: "amd64".into(), ..Default::default() }
            .into(),
        ..Default::default()
    };
    let message =
        agent::AgentMessage { message: Some(Up::StepRequest(Box::new(up))), ..Default::default() };
    s.send(message).await.unwrap();
    let pick = |d: &Down| match d {
        Down::StepResolution(r) if r.request == request => r.outcome.clone(),
        _ => None,
    };
    until(s, pick).await
}

/// The plan's install step and the index of its provider action.
fn provider_action(plan: &pb::Plan) -> (u32, u32) {
    let install = plan.steps.iter().find(|s| s.action == "install").expect("an install step");
    let Some(Kind::ListValue(actions)) = install.detail.get("actions").and_then(|v| v.kind.clone())
    else {
        panic!("its actions: {:?}", install.detail)
    };
    let verb = |a: &pb::Value| match &a.kind {
        Some(Kind::MapValue(m)) => m.fields.get("verb").and_then(|v| v.kind.clone()),
        _ => None,
    };
    let at = actions
        .values
        .iter()
        .position(|a| matches!(verb(a), Some(Kind::StringValue(v)) if v == "provider"))
        .expect("a provider action");
    (install.index, u32::try_from(at).unwrap())
}

/// Cordons every node but `keep`, so the workload lands there.
async fn only(h: &Harness, keep: &str) {
    let listed = pb::ListNodesRequest { organization: h.root_org(), ..Default::default() };
    let nodes = h.nodes(h.root()).list_nodes(listed).await.unwrap().into_owned().nodes;
    for n in nodes.iter().filter(|n| n.name != keep) {
        let r = pb::CordonNodeRequest {
            mutation: mutation(&format!("cordon-{}", n.id)).into(),
            node: n.id.clone(),
            ..Default::default()
        };
        h.nodes(h.root()).cordon_node(r).await.unwrap();
    }
}

#[tokio::test]
async fn a_node_resolves_its_install_s_provider_step_and_a_channel_marks_the_workload() {
    let h = Harness::new().await;
    let steam = serving(&h).await;
    let node = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, node.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    only(&h, "fra-3").await;
    let project = project(&h, &h.root_org(), "games").await;
    let repo = push_paper_like(&with_step("valve/steam.app: {to: /data/server, appid: 730}")).await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("d1", &project, "cs", body(&repo, &[])));
    let declared = declared.await.unwrap().into_owned();
    let (workload, plan) =
        (declared.workload.as_option().unwrap().id.clone(), declared.plan.as_option().unwrap());
    let (step, action) = provider_action(plan);
    until(&mut s, assignment(|a| a.plan.is_set())).await;

    let Outcome::Result(resolved) = resolve(&mut s, "r1", (&plan.id, step, action)).await else {
        panic!("resolved")
    };
    let artifacts = &resolved.artifacts;
    assert_eq!((artifacts.len(), artifacts[0].path.as_str(), resolved.pinned), (1, "game", false));
    let get = pb::GetWorkloadRequest { id: workload.clone(), ..Default::default() };
    let read = workloads.get_workload(get).await.unwrap().into_owned();
    let status = read.status.as_option().unwrap();
    let marked = status.workload.as_option().unwrap().unreproducible.as_option().unwrap();
    assert!(marked.reason.contains("public at 7346914270000188842"), "{}", marked.reason);

    let refused = |o: Outcome| match o {
        Outcome::Refused(e) => e.reason,
        other @ Outcome::Result(_) => panic!("{other:?}"),
    };
    let wrong = resolve(&mut s, "r2", (&plan.id, step, action - 1)).await;
    assert_eq!(refused(wrong), "step.not_a_step", "the eula is written, not resolved");

    let gone = pb::UninstallExtensionRequest {
        mutation: mutation("u1").into(),
        id: steam.id.clone(),
        ..Default::default()
    };
    h.extensions(h.root()).uninstall_extension(gone).await.unwrap();
    let changed = resolve(&mut s, "r3", (&plan.id, step, action)).await;
    assert_eq!(refused(changed), "step.provider_changed", "only what the intent pinned answers");
}
