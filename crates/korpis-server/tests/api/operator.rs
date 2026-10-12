// SPDX-License-Identifier: AGPL-3.0-or-later

//! Operator fields (§5 of docs/09-recipes.md): a tenant who may change its workload may not set
//! a field the recipe reserves for whoever is above its organization.

use crate::recipes::{body, create, project, push_paper, text};
use crate::support::{Harness, detail, mutation, org_scope, reason};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;

#[tokio::test]
async fn an_operator_field_needs_authority_above_the_tenant() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let tenant = h
        .orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation("org").into(),
            name: "tenant".into(),
            parent: Some(h.root_org()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned()
        .organization
        .as_option()
        .unwrap()
        .id
        .clone();
    let project = project(&h, &tenant, "games").await;
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("grant").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec![
                "workload.create".into(),
                "workload.write".into(),
                "workload.read".into(),
            ],
            scope: org_scope(&tenant).into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let tenant_client = h.workloads(h.config(Some(secret.expose())));
    let password = [("rcon_password", text("hunter2"))];
    let e = tenant_client
        .declare_intent(create("t1", &project, "mc", body(&repo, &password)))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "authorization.field_denied");
    assert_eq!(detail(&e).field, "body.config.rcon_password");
    assert!(
        h.effects("workload.write").await.iter().any(|(o, _)| o == "denied"),
        "the refusal is an effect"
    );
    // The same declaration without the operator field is the tenant's to make, and the
    // operator above may set it.
    tenant_client.declare_intent(create("t2", &project, "mc", body(&repo, &[]))).await.unwrap();
    h.workloads(h.root())
        .declare_intent(create("r1", &project, "mc2", body(&repo, &password)))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_caller_without_authority_is_refused_on_the_record() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let (token, secret) = h.bare_token().await;
    h.issue_to(token, "read-only", &["workload.read", "project.read"]).await;
    let e = h
        .workloads(h.config(Some(secret.expose())))
        .declare_intent(create("d1", &project, "mc", body(&repo, &[])))
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "authorization.denied");
    let denied = h.effects("workload.create").await;
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].0, "denied");
}

#[tokio::test]
async fn a_propose_only_grant_produces_a_plan_that_waits_for_another_subject() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("grant").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["workload.create".into(), "workload.read".into()],
            scope: org_scope(&h.root_org()).into(),
            conditions: pb::Conditions {
                requires_approval_by: org_scope(&h.root_org()).into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let declared = h
        .workloads(h.config(Some(secret.expose())))
        .declare_intent(create("d1", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let plan = declared.plan.as_option().unwrap();
    assert_eq!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_AUTHORITY));
    assert!(plan.approval_scope.is_set());
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED));
    assert_eq!(declared.workload.as_option().unwrap().target, None, "nothing moved");
}
