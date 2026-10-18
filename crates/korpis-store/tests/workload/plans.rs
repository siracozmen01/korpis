// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans: stored, read back, decided by compare-and-swap, superseded (§3.3 of
//! docs/01-model.md).

use crate::support::{Fixture, alice, create, fixture, now};
use korpis_app::Page;
use korpis_app::ports::{Store, Tx, WorkloadRead, WorkloadWrite};
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::recipe::Disruption;
use korpis_core::workload::plan::compute::intent_steps;
use korpis_core::workload::plan::impact::{assess, delta};
use korpis_core::workload::plan::{
    Approval, Classifier, DriverClass, PROPOSAL_LIFETIME, PlanStatus,
};
use korpis_core::workload::{Intent, Plan, PlanTarget, PowerState};
use uuid::Uuid;

pub fn plan(f: &Fixture, to: &Intent, approval: Approval) -> Plan {
    let (driver, recipe) = (DriverClass::default(), Disruption::default());
    let classifier = Classifier { driver: &driver, recipe: &recipe, overrides: &[] };
    let steps =
        intent_steps(to.workload, (None, &to.body), None, PowerState::Running, classifier).unwrap();
    let impact = assess(&steps, delta(None, Some(&to.body)));
    let now = now();
    Plan {
        id: Uuid::now_v7(),
        workload: to.workload,
        target: PlanTarget::Intent(to.id),
        operation: None,
        from_intent: None,
        from_power: 0,
        computed_at: now,
        computed_by: alice(),
        proposed_under: Some(f.grant),
        steps,
        impact,
        approval,
        approval_reason: "because".into(),
        status: PlanStatus::Proposed,
        completed: None,
        apply_after: None,
        expires_at: now + PROPOSAL_LIFETIME,
        approved_by: None,
        approved_at: None,
    }
}

#[tokio::test]
async fn a_plan_reads_back_with_its_steps_impact_and_approval() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let scope = Scope::Object(ObjectRef::new(Kind::Organization, f.organization));
    let p = plan(&f, &first, Approval::Authority { scope });
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_plan(&p).await.unwrap();
    tx.commit().await.unwrap();
    let read = f.store.plan(p.id).await.unwrap().unwrap();
    assert_eq!(read, p);
    assert!(!read.steps.is_empty());
    assert_eq!(f.store.plans(w.id, Page::default()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_decision_is_a_compare_and_swap_on_the_status() {
    let f = fixture().await;
    let (_, first) = create(&f, "mc").await;
    let mut p = plan(&f, &first, Approval::Confirmation);
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_plan(&p).await.unwrap();
    let locked = tx.lock_plan(p.id).await.unwrap().unwrap();
    assert_eq!(locked.status, PlanStatus::Proposed);
    p.transition(PlanStatus::Approved).unwrap();
    p.approved_by = Some(alice());
    p.approved_at = Some(now());
    assert!(tx.update_plan(&p, PlanStatus::Proposed).await.unwrap());
    assert!(!tx.update_plan(&p, PlanStatus::Proposed).await.unwrap(), "already decided");
    tx.commit().await.unwrap();
    let read = f.store.plan(p.id).await.unwrap().unwrap();
    assert_eq!((read.status, read.approved_by), (PlanStatus::Approved, Some(alice())));
}

#[tokio::test]
async fn a_newer_declaration_supersedes_every_proposed_plan_but_its_own() {
    let f = fixture().await;
    let (_, first) = create(&f, "mc").await;
    let (old, new) = (plan(&f, &first, Approval::Confirmation), plan(&f, &first, Approval::Auto));
    let mut power = plan(&f, &first, Approval::Confirmation);
    power.target = PlanTarget::Power(1);
    let mut tx = f.store.begin().await.unwrap();
    tx.insert_plan(&old).await.unwrap();
    tx.insert_plan(&power).await.unwrap();
    tx.insert_plan(&new).await.unwrap();
    assert_eq!(tx.supersede_proposed(&new).await.unwrap(), 1, "only plans of its own kind");
    tx.commit().await.unwrap();
    assert_eq!(f.store.plan(power.id).await.unwrap().unwrap().status, PlanStatus::Proposed);
    assert_eq!(f.store.plan(old.id).await.unwrap().unwrap().status, PlanStatus::Superseded);
    assert_eq!(f.store.plan(new.id).await.unwrap().unwrap().status, PlanStatus::Proposed);
}
