// SPDX-License-Identifier: AGPL-3.0-or-later

//! The plan lifecycle (§3.3 of docs/01-model.md, §3.2 and §7 of docs/03-state.md).

use crate::lifecycle::{approve, pointers, power, workload};
use crate::recipes::{body, change, text};
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::v1 as pb;

fn status(p: &pb::Plan) -> Option<pb::PlanStatus> {
    p.status.as_known()
}

#[tokio::test]
async fn a_disruptive_stop_waits_and_reaches_nodes_only_when_confirmed() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    assert_eq!(pointers(&h, &w).await, (true, 1, 1), "the first approval approved power 1");
    let workloads = h.workloads(h.root());
    let stopped = workloads.stop(power("s1", &w, 1)).await.unwrap().into_owned();
    let plan = stopped.plan.as_option().unwrap();
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_CONFIRMATION));
    assert_eq!(status(plan), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED));
    assert_eq!(pointers(&h, &w).await, (true, 2, 1), "declared, not yet what nodes are told");
    let e = workloads.stop(power("s2", &w, 1)).await.unwrap_err();
    assert_eq!(reason(&e), "workload.power_moved");
    // The proposer may confirm their own plan (§3.3 of docs/01-model.md).
    let confirmed =
        h.plans(h.root()).approve_plan(approve("a1", &plan.id)).await.unwrap().into_owned();
    assert_eq!(
        status(confirmed.plan.as_option().unwrap()),
        Some(pb::PlanStatus::PLAN_STATUS_APPROVED)
    );
    assert_eq!(pointers(&h, &w).await, (true, 2, 2));
    // Starting is not disruptive for this recipe: approved at once.
    let started = workloads
        .start(pb::StartRequest {
            mutation: power("s3", &w, 2).mutation,
            workload: w.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(
        status(started.plan.as_option().unwrap()),
        Some(pb::PlanStatus::PLAN_STATUS_APPROVED)
    );
    assert_eq!(pointers(&h, &w).await, (true, 3, 3));
}

#[tokio::test]
async fn a_rejected_plan_moves_nothing_and_cannot_then_be_approved() {
    let h = Harness::new().await;
    let (w, repo) = workload(&h).await;
    let hard = body(&repo, &[("difficulty", text("hard"))]);
    let declared =
        h.workloads(h.root()).declare_intent(change("d", &w, 1, hard)).await.unwrap().into_owned();
    let plan = declared.plan.as_option().unwrap().id.clone();
    let plans = h.plans(h.root());
    let rejected = plans
        .reject_plan(pb::RejectPlanRequest {
            mutation: mutation("r").into(),
            plan: plan.clone(),
            reason: "not during the event".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(
        status(rejected.plan.as_option().unwrap()),
        Some(pb::PlanStatus::PLAN_STATUS_REJECTED)
    );
    let e = plans.approve_plan(approve("a", &plan)).await.unwrap_err();
    assert_eq!(reason(&e), "plan.not_proposed");
    assert_eq!(h.effects("plan.reject").await.len(), 1);
}

#[tokio::test]
async fn a_newer_declaration_supersedes_and_an_old_plan_expires() {
    let h = Harness::new().await;
    let (w, repo) = workload(&h).await;
    let workloads = h.workloads(h.root());
    let first = workloads
        .declare_intent(change("d1", &w, 1, body(&repo, &[("difficulty", text("hard"))])))
        .await
        .unwrap()
        .into_owned();
    let second = workloads
        .declare_intent(change("d2", &w, 2, body(&repo, &[("difficulty", text("easy"))])))
        .await
        .unwrap()
        .into_owned();
    let plans = h.plans(h.root());
    let old = first.plan.as_option().unwrap().id.clone();
    let got = plans
        .get_plan(pb::GetPlanRequest { id: old.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(status(got.plan.as_option().unwrap()), Some(pb::PlanStatus::PLAN_STATUS_SUPERSEDED));
    let newer = second.plan.as_option().unwrap().id.clone();
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE plan SET expires_at = now() - interval '1 minute'")
        .execute(store.pool())
        .await
        .unwrap();
    let e = plans.approve_plan(approve("a", &newer)).await.unwrap_err();
    assert_eq!(reason(&e), "plan.expired");
    assert_eq!(h.effects("plan.expire").await.len(), 1, "the expiry is on the record");
    let listed = plans
        .list_plans(pb::ListPlansRequest { workload: w.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(listed.plans.len(), 3);
}

#[tokio::test]
async fn cancelling_an_approved_plan_before_it_starts_puts_the_targets_back() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let plans = h.plans(h.root());
    let listed = plans
        .list_plans(pb::ListPlansRequest { workload: w.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    let first = listed.plans[0].id.clone();
    let cancelled = plans
        .cancel_plan(pb::CancelPlanRequest {
            mutation: mutation("c").into(),
            plan: first,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(
        status(cancelled.plan.as_option().unwrap()),
        Some(pb::PlanStatus::PLAN_STATUS_CANCELLED)
    );
    assert_eq!(pointers(&h, &w).await, (false, 1, 0), "as if it had never been approved");
}

#[tokio::test]
async fn a_proposed_declaration_waits_to_be_confirmed_even_when_it_could_apply_at_once() {
    use crate::recipes::{create, project, push_paper};
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let mut request = create("w", &project, "mc", body(&repo, &[]));
    request.propose = true;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(request.clone()).await.unwrap().into_owned();
    let plan = declared.plan.as_option().unwrap();
    let w = declared.workload.as_option().unwrap().id.clone();
    assert_eq!(status(plan), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "persisted, waiting");
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_CONFIRMATION));
    assert!(!pointers(&h, &w).await.0, "nothing is told to a node yet");
    let plans = h.plans(h.root());
    let got = plans.get_plan(pb::GetPlanRequest { id: plan.id.clone(), ..Default::default() });
    assert!(got.await.is_ok(), "a real plan, with an identifier that can be applied");
    plans.approve_plan(approve("a", &plan.id)).await.unwrap();
    assert!(pointers(&h, &w).await.0, "confirmed, and the target moved");

    let mut both = create("w2", &project, "mc2", body(&repo, &[]));
    (both.propose, both.dry_run) = (true, true);
    let e = workloads.declare_intent(both).await.unwrap_err();
    assert_eq!(reason(&e), "request.invalid");
}

#[tokio::test]
async fn a_proposed_power_change_waits_to_be_confirmed_even_when_it_could_apply_at_once() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let workloads = h.workloads(h.root());
    let stopped = workloads.stop(power("s1", &w, 1)).await.unwrap().into_owned();
    let stop = stopped.plan.as_option().unwrap().id.clone();
    h.plans(h.root()).approve_plan(approve("a1", &stop)).await.unwrap();
    let start = pb::StartRequest {
        mutation: power("s2", &w, 2).mutation,
        workload: w.clone(),
        propose: true,
        ..Default::default()
    };
    let proposed = workloads.start(start).await.unwrap().into_owned();
    let plan = proposed.plan.as_option().unwrap();
    assert_eq!(status(plan), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "a start would not wait");
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_CONFIRMATION));
    assert_eq!(pointers(&h, &w).await, (true, 3, 2), "declared, not yet what nodes are told");
    h.plans(h.root()).approve_plan(approve("a2", &plan.id)).await.unwrap();
    assert_eq!(pointers(&h, &w).await, (true, 3, 3), "confirmed, it applies");
}
