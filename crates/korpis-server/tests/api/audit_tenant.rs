// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tenant-visible audit (§4 of docs/15-observability.md): an organization reads its own
//! subtree's effects, each only if the caller may read what it is about, never a sibling's; a
//! reseller reads its customers'; and its pages are sealed as every filtered list's are.

use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::principal;

/// A new organization under `parent`; its identifier.
async fn org(h: &Harness, parent: &str, name: &str) -> String {
    let made = pb::CreateOrganizationRequest {
        mutation: mutation(&format!("org-{name}")).into(),
        name: name.into(),
        parent: Some(parent.to_owned()),
        ..Default::default()
    };
    let made = h.orgs(h.root()).create_organization(made).await.unwrap().into_owned();
    made.organization.into_option().unwrap().id
}

async fn project(h: &Harness, organization: &str, name: &str) {
    let made = pb::CreateProjectRequest {
        mutation: mutation(&format!("project-{name}")).into(),
        organization: organization.to_owned(),
        name: name.into(),
        ..Default::default()
    };
    h.projects(h.root()).create_project(made).await.unwrap();
}

/// A token holding `actions` on `organization`; its secret.
async fn holding(h: &Harness, organization: &str, actions: &[&str]) -> String {
    let (token, secret) = h.bare_token().await;
    let issue = pb::IssueGrantRequest {
        mutation: mutation(&format!("to-{token}")).into(),
        parent: h.root_grant(),
        subject: principal::to_pb(&Principal::Token(token)).into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope: org_scope(organization).into(),
        ..Default::default()
    };
    h.grants(h.root()).issue_grant(issue).await.unwrap();
    secret.expose().to_owned()
}

/// The actions of every effect `secret` reads of `organization`, a page of `size` at a time.
async fn read(h: &Harness, secret: &str, organization: &str, size: u32) -> Vec<String> {
    let log = h.effect_log(h.config(Some(secret)));
    let (mut actions, mut token) = (Vec::new(), String::new());
    loop {
        let asked = pb::ListEffectsRequest {
            organization: Some(organization.to_owned()),
            page: pb::PageRequest { page_size: size, page_token: token, ..Default::default() }
                .into(),
            ..Default::default()
        };
        let page = log.list_effects(asked).await.unwrap().into_owned();
        actions.extend(page.effects.into_iter().map(|e| e.action));
        token = page.page.into_option().unwrap_or_default().next_page_token;
        if token.is_empty() {
            return actions;
        }
    }
}

#[tokio::test]
async fn an_organization_reads_its_subtree_s_effects_as_far_as_its_grants_reach() {
    let h = Harness::new().await;
    let reseller = org(&h, &h.root_org(), "reseller").await;
    let customer = org(&h, &reseller, "customer").await;
    let sibling = org(&h, &reseller, "sibling").await;
    project(&h, &customer, "survival").await;
    project(&h, &sibling, "creative").await;
    let tenant = holding(&h, &customer, &["org.read", "project.read"]).await;

    // Its own organization's creation and project, and the grant it holds under grant.read
    // only; the sibling's project never.
    let seen = read(&h, &tenant, &customer, 100).await;
    assert_eq!(seen, ["org.create", "project.create"], "no grant.issued without grant.read");
    let auditor = holding(&h, &customer, &["org.read", "project.read", "grant.read"]).await;
    let seen = read(&h, &auditor, &customer, 1).await;
    assert!(
        seen.contains(&"grant.issued".to_owned()),
        "under grant.read, a page at a time: {seen:?}"
    );

    // A denial against what it holds is its to see; another organization's log is not found.
    let ceiling = pb::SetDelegationCeilingRequest {
        mutation: mutation("ceiling").into(),
        organization: customer.clone(),
        ..Default::default()
    };
    let refused = h.orgs(h.config(Some(&tenant))).set_delegation_ceiling(ceiling).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::PermissionDenied, "it lacks org.write");
    let seen = read(&h, &tenant, &customer, 100).await;
    assert!(
        seen.contains(&"org.write".to_owned()),
        "the denial, as the action it lacked: {seen:?}"
    );
    let other =
        pb::ListEffectsRequest { organization: Some(sibling.clone()), ..Default::default() };
    let hidden = h.effect_log(h.config(Some(&tenant))).list_effects(other).await.unwrap_err();
    assert_eq!(hidden.code, ErrorCode::NotFound, "a sibling's log is not its to know of");

    // The reseller reads both customers' projects, beside its own creation, and asks by action.
    let reseller_reader = holding(&h, &reseller, &["org.read", "project.read"]).await;
    let seen = read(&h, &reseller_reader, &reseller, 2).await;
    let projects = seen.iter().filter(|a| *a == "project.create").count();
    assert_eq!(projects, 2, "both customers' projects: {seen:?}");
    let by_action = pb::ListEffectsRequest {
        organization: Some(reseller.clone()),
        action_prefix: "project.".into(),
        ..Default::default()
    };
    let log = h.effect_log(h.config(Some(&reseller_reader)));
    let only = log.list_effects(by_action).await.unwrap().into_owned();
    assert!(only.effects.iter().all(|e| e.action == "project.create") && only.effects.len() == 2);
    let about = only.effects[0].organization.clone();
    assert!(about == Some(customer.clone()) || about == Some(sibling.clone()), "{about:?}");

    // A page token is sealed: it names no effect, and a forged one is refused.
    let forged = pb::ListEffectsRequest {
        organization: Some(customer),
        page: pb::PageRequest { page_token: "1.abc".into(), ..Default::default() }.into(),
        ..Default::default()
    };
    let refused = h.effect_log(h.config(Some(&tenant))).list_effects(forged).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
}
