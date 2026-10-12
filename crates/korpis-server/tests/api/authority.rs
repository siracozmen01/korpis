// SPDX-License-Identifier: AGPL-3.0-or-later

//! Approval that needs someone else's authority (§3.6 of docs/08-identity.md).

use crate::lifecycle::{approve, pointers};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation, org_scope, reason};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;

#[tokio::test]
async fn nobody_approves_their_own_proposal_and_an_approver_can() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("grant").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["workload.create".into(), "workload.read".into(), "plan.approve".into()],
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
    let proposer = h.config(Some(secret.expose()));
    let declared = h
        .workloads(proposer.clone())
        .declare_intent(create("d", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let plan = declared.plan.as_option().unwrap().id.clone();
    let w = declared.workload.as_option().unwrap().id.clone();
    let e = h.plans(proposer).approve_plan(approve("self", &plan)).await.unwrap_err();
    assert_eq!(reason(&e), "authorization.denied", "an approval cannot be self-supplied");
    assert_eq!(pointers(&h, &w).await, (false, 1, 0));
    h.plans(h.root()).approve_plan(approve("root", &plan)).await.unwrap();
    assert_eq!(pointers(&h, &w).await, (true, 1, 1));
}
