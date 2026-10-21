// SPDX-License-Identifier: AGPL-3.0-or-later

//! What of the effect log a tenant reads (§4 of docs/15-observability.md): an effect about its
//! workload without the operator's node it landed on, an enrollment without its node's name,
//! and a person signed in through their set reading under the set's grant while the set has
//! them.

use crate::external_complete::signed_in;
use crate::external_sign_in::{GONE, issuing};
use crate::member_issue::{ops_read, verify};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::principal;

#[tokio::test]
async fn a_tenant_reads_its_workload_s_effects_without_the_operator_s_node() {
    let h = Harness::new().await;
    let node = h.node(None, "fra-4", &[]).await;
    let made = pb::CreateOrganizationRequest {
        mutation: mutation("org-blockhaven").into(),
        name: "blockhaven".into(),
        parent: Some(h.root_org()),
        ..Default::default()
    };
    let made = h.orgs(h.root()).create_organization(made).await.unwrap().into_owned();
    let tenant_org = made.organization.into_option().unwrap().id;
    let survival = project(&h, &tenant_org, "survival").await;
    let repo = push_paper().await;
    let declared = create("w", &survival, "mc", body(&repo, &[]));
    h.workloads(h.root()).declare_intent(declared).await.unwrap();

    let (token, secret) = h.bare_token().await;
    let issue = pb::IssueGrantRequest {
        mutation: mutation("tenant").into(),
        parent: h.root_grant(),
        subject: principal::to_pb(&Principal::Token(token)).into(),
        actions: vec!["org.read".into(), "project.read".into(), "workload.read".into()],
        scope: org_scope(&tenant_org).into(),
        ..Default::default()
    };
    h.grants(h.root()).issue_grant(issue).await.unwrap();
    let asked = || pb::ListEffectsRequest {
        organization: Some(tenant_org.clone()),
        action_prefix: "workload.place".into(),
        ..Default::default()
    };
    let as_root = h.effect_log(h.root()).list_effects(asked()).await.unwrap().into_owned();
    assert_eq!(as_root.effects.len(), 1, "the workload was placed once");
    assert_eq!(as_root.effects[0].node.as_deref(), Some(node.as_str()), "the operator sees where");
    let tenant = h.effect_log(h.config(Some(secret.expose())));
    let seen = tenant.list_effects(asked()).await.unwrap().into_owned();
    assert_eq!(seen.effects.len(), 1, "the tenant reads that its workload was placed");
    let placed = format!("{:?}", seen.effects[0]);
    assert!(!placed.contains(&node), "and not on which of the operator's nodes: {placed}");

    // A machine enrolled into the tenant's organization is said without its name to a reader
    // who may not read the tenant's nodes, though the enrollment is about the organization.
    let enrol = pb::CreateEnrollmentTokenRequest {
        mutation: mutation("enrol").into(),
        organization: tenant_org.clone(),
        node_name: "blockhaven-1".into(),
        ..Default::default()
    };
    h.nodes(h.root()).create_enrollment_token(enrol).await.unwrap();
    let enrolled = || pb::ListEffectsRequest {
        organization: Some(tenant_org.clone()),
        action_prefix: "node.enrollment".into(),
        ..Default::default()
    };
    let seen = tenant.list_effects(enrolled()).await.unwrap().into_owned();
    assert_eq!(seen.effects.len(), 1, "the tenant reads that a machine was enrolled");
    let said = format!("{:?}", seen.effects[0]);
    assert!(!said.contains("blockhaven-1"), "and not which: {said}");
    let as_root = h.effect_log(h.root()).list_effects(enrolled()).await.unwrap().into_owned();
    assert!(format!("{:?}", as_root.effects[0]).contains("blockhaven-1"), "the operator does");
}

#[tokio::test]
async fn a_person_reads_a_project_s_effects_through_their_set_until_they_leave_it() {
    let h = Harness::new().await;
    let _corp = issuing(&h, 76, "corp", true).await;
    let survival = project(&h, &h.root_org(), "survival").await;
    let (_, binding) = ops_read(&h, &survival).await;
    let (_, token) = signed_in(&h, "fern", "ops", "p1").await;
    let asked = || pb::ListEffectsRequest { project: Some(survival.clone()), ..Default::default() };
    let log = h.effect_log(h.config(Some(&token)));

    let seen = log.list_effects(asked()).await.expect("the set's grant reads the project");
    let seen = seen.into_owned();
    assert!(seen.effects.iter().any(|e| e.action == "project.create"), "{:?}", seen.effects);

    GONE.lock().unwrap().push("fern".into());
    verify(&h, &binding).await;
    let left = log.list_effects(asked()).await.unwrap_err();
    assert_eq!(left.code, ErrorCode::NotFound, "fern left ops, and reads it no more");
}
