// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declaring intents (§3.3 of docs/01-model.md, §3.1 of docs/09-recipes.md).

use crate::recipes::{body, change, create, project, push_paper, text};
use crate::support::{Harness, detail, reason};
use korpis_core::workload::plan::StepAction;
use korpis_core::workload::spec::IsolationTier;
use korpis_proto::korpis::v1 as pb;

const GIB: u64 = 1 << 30;

#[tokio::test]
async fn a_new_workload_is_completed_by_its_recipe_and_approved_when_nothing_needs_a_human() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("d1", &project, "mc", body(&repo, &[("motd", text("hi"))])))
        .await
        .unwrap()
        .into_owned();
    let (w, intent, plan) = (
        declared.workload.as_option().unwrap(),
        declared.intent.as_option().unwrap(),
        declared.plan.as_option().unwrap(),
    );
    let b = intent.body.as_option().unwrap();
    // Completed from the preset and the probes (§3.1 of docs/09-recipes.md).
    assert!(b.recipe.as_option().unwrap().digest.starts_with("sha256:"));
    let runtime = b.runtime.as_option().unwrap();
    assert_eq!(runtime.driver, "oci");
    let memory = b.resources.as_option().unwrap().memory.as_option().unwrap();
    assert_eq!(memory.reservation, 4 * GIB, "the preset's recommendation");
    assert_eq!(b.endpoints.len(), 3);
    assert_eq!(b.volumes[0].path, "/data");
    assert!(b.health.as_option().unwrap().readiness.is_set());
    // Configuration resolved: defaults written, derived values computed (K-17).
    assert!(b.config.contains_key("difficulty"));
    assert!(b.config.contains_key("java_heap"));
    // Nothing disruptive or irreversible: approved now, and the target moved.
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_AUTO));
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_APPROVED));
    assert_eq!(w.target.as_deref(), Some(intent.id.as_str()));
    let actions: Vec<_> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["pull_recipe", "create_volume", "install", "create", "start"]);
    // Every verb, then the template, into the volume the plan creates: it writes over nothing,
    // so it can be undone (§4 of docs/09-recipes.md).
    let install = &plan.steps[2];
    assert_eq!(installed(install), ("create".to_owned(), 4));
    assert!(install.reversible);
    for action in ["workload.create", "plan.approve"] {
        let rows = h.effects(action).await;
        assert_eq!(rows.len(), 1, "{action}");
        assert_eq!(rows[0].1, Some(h.installed.root_grant), "{action} names its grant");
    }
}

#[tokio::test]
async fn a_disruptive_change_waits_and_the_head_moves_without_the_target() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let first = workloads
        .declare_intent(create("d1", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let w = first.workload.as_option().unwrap();
    let hard = body(&repo, &[("difficulty", text("hard"))]);
    let second =
        workloads.declare_intent(change("d2", &w.id, 1, hard.clone())).await.unwrap().into_owned();
    let plan = second.plan.as_option().unwrap();
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_CONFIRMATION));
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED));
    let reconfigure = plan.steps.iter().find(|s| s.action == "reconfigure").unwrap();
    assert!(reconfigure.disruptive, "difficulty is a disruptive field of the recipe");
    // The template that uses difficulty is rendered again, with the instance stopped around it,
    // over what is there: that cannot be undone.
    let actions: Vec<_> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["stop", "install", "reconfigure", "start"]);
    let install = &plan.steps[1];
    assert_eq!(installed(install), ("config".to_owned(), 1));
    assert!(!install.reversible);
    assert_eq!(reconfigure.classified_by.as_known(), Some(pb::StepLayer::STEP_LAYER_RECIPE));
    let moved = second.workload.as_option().unwrap();
    assert_eq!(moved.intent, second.intent.as_option().unwrap().id);
    assert_eq!(moved.target, w.target, "declaring does not move the target");
    // Declared against a head that has moved: a conflict, never last-write-wins.
    let e = workloads.declare_intent(change("d3", &w.id, 1, hard)).await.unwrap_err();
    assert_eq!(reason(&e), "workload.intent_moved");
}

#[tokio::test]
async fn retries_replay_and_a_dry_run_persists_nothing() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let mut dry = create("dry", &project, "mc", body(&repo, &[]));
    dry.dry_run = true;
    let planned = workloads.declare_intent(dry).await.unwrap().into_owned();
    assert!(planned.plan.is_set());
    assert!(h.effects("workload.create").await.is_empty(), "a dry run records nothing");
    let request = create("d1", &project, "mc", body(&repo, &[]));
    let a = workloads.declare_intent(request.clone()).await.unwrap().into_owned();
    let b = workloads.declare_intent(request).await.unwrap().into_owned();
    assert_eq!(a.plan.as_option().unwrap().id, b.plan.as_option().unwrap().id);
    let e =
        workloads.declare_intent(create("d2", &project, "mc", body(&repo, &[]))).await.unwrap_err();
    assert_eq!(reason(&e), "workload.name_taken");
}

#[tokio::test]
async fn configuration_the_recipe_does_not_allow_is_refused_with_its_field() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    for (key, value, field) in [
        ("typo", text("x"), "body.config.typo"),
        ("difficulty", text("impossible"), "body.config.difficulty"),
        ("java_heap", text("1Gi"), "body.config.java_heap"),
    ] {
        let e = workloads
            .declare_intent(create(key, &project, "mc", body(&repo, &[(key, value)])))
            .await
            .unwrap_err();
        assert_eq!(detail(&e).field, field, "{key}: {e:?}");
    }
}

#[tokio::test]
async fn a_dependency_is_refused_until_something_resolves_it() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let mut b = body(&repo, &[]);
    b.dependencies.push(pb::Dependency {
        name: "db".into(),
        target: Some(korpis_proto::korpis::v1::__buffa::oneof::dependency::Target::Workload(
            "wl_00000000000000000000000000000001".into(),
        )),
        ..Default::default()
    });
    let e = h.workloads(h.root()).declare_intent(create("d", &project, "web", b)).await;
    let e = e.unwrap_err();
    assert_eq!(e.code, connectrpc::ErrorCode::Unimplemented, "{e:?}");
    assert!(e.message.as_deref().unwrap_or_default().contains("Phase 1"), "{e:?}");
}

#[tokio::test]
async fn a_driver_that_declares_starting_disruptive_on_its_tier_makes_a_human_confirm_it() {
    let h = Harness::new().await;
    let org = h.installed.organization.id;
    let mut node = korpis_store::testing::ready_node(org, "slow-boot", &[]).unwrap();
    node.drivers[0].capabilities.disruptive = vec![
        (IsolationTier::Vm, vec![StepAction::Create]),
        (IsolationTier::Container, vec![StepAction::Start]),
    ];
    h.enroll(node).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("d1", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let plan = declared.plan.as_option().unwrap();
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_CONFIRMATION));
    let layer = |action: &str| {
        let step = plan.steps.iter().find(|s| s.action == action).unwrap();
        (step.disruptive, step.classified_by.as_known())
    };
    assert_eq!(layer("start"), (true, Some(pb::StepLayer::STEP_LAYER_DRIVER)));
    assert_eq!(
        layer("create"),
        (false, Some(pb::StepLayer::STEP_LAYER_VOCABULARY)),
        "what a driver declares for another tier does not reach this one"
    );
}

/// Why an install step runs, and how many actions it carries.
fn installed(s: &pb::Step) -> (String, usize) {
    use korpis_proto::korpis::v1::__buffa::oneof::value::Kind;
    let reason = match s.detail.get("reason").and_then(|v| v.kind.as_ref()) {
        Some(Kind::StringValue(r)) => r.clone(),
        other => panic!("no reason: {other:?}"),
    };
    let actions = match s.detail.get("actions").and_then(|v| v.kind.as_ref()) {
        Some(Kind::ListValue(l)) => l.values.len(),
        other => panic!("no actions: {other:?}"),
    };
    (reason, actions)
}
