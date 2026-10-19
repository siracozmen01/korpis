// SPDX-License-Identifier: AGPL-3.0-or-later

//! Issuing from a grant template (§4 of docs/08-identity.md), on a real control plane: a reseller
//! applies its Moderator at a customer's project, and the grant is the template's actions and
//! delegation on that project, expiring after its lifetime, named after it, and carrying the
//! version it came from. Editing the template afterwards widens nothing already issued, and an
//! issue made against the old version is a conflict. The template grants nobody anything: one
//! naming an action the issuer cannot pass on is refused as a hand-written request would be.

use crate::delegation::{Holder, delegate, delegation, organization};
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::{principal, scope};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const RESELLER: [&str; 7] = [
    "template.write",
    "org.read",
    "org.create",
    "project.create",
    "project.read",
    "workload.restart",
    "workload.console.read",
];
const PASSED: [&str; 3] = ["project.read", "workload.restart", "workload.console.read"];

fn template(name: &str, actions: &[&str], kind: pb::ObjectKind) -> pb::GrantTemplateBody {
    pb::GrantTemplateBody {
        name: name.into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope_pattern: pb::ScopePattern { kind: kind.into(), ..Default::default() }.into(),
        delegation: delegation(Some(1), &["workload.restart"]).into(),
        conditions: pb::TemplateConditions {
            lifetime: Some(korpis_server::api::wire::duration(Duration::from_secs(86_400))).into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// The reseller, and the project of its customer a moderator is wanted for.
async fn reseller_and_project(harness: &Harness) -> (Holder, String, String) {
    let host = Holder { grant: harness.root_grant(), config: harness.root(), token: Uuid::nil() };
    let org = organization(harness, &host, &harness.root_org(), "reseller").await;
    let ask = (&RESELLER[..], org_scope(&org));
    let reseller =
        delegate(harness, (&host.grant, harness.root()), ask, delegation(Some(2), &PASSED)).await;
    let customer = organization(harness, &reseller, &org, "customer").await;
    let made = harness
        .projects(reseller.config.clone())
        .create_project(pb::CreateProjectRequest {
            mutation: mutation("survival").into(),
            organization: customer,
            name: "survival".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    (reseller, org, made.project.as_option().unwrap().id.clone())
}

fn project_scope(id: &str) -> pb::Scope {
    let id = Id::parse(IdKind::Project, id).unwrap().uuid();
    scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Project, id)))
}

/// Issuing from `template` at `version` on `at`, by `reseller`, to a new token.
async fn from(
    harness: &Harness,
    reseller: &Holder,
    (template, version): (&str, u64),
    at: pb::Scope,
) -> Result<pb::Grant, connectrpc::ConnectError> {
    let (token, _) = harness.bare_token().await;
    let request = pb::IssueGrantRequest {
        mutation: mutation(&format!("issue-{}", Uuid::now_v7())).into(),
        parent: reseller.grant.clone(),
        subject: principal::to_pb(&Principal::Token(token)).into(),
        scope: at.into(),
        template: pb::TemplateRef { template: template.into(), version, ..Default::default() }
            .into(),
        ..Default::default()
    };
    let issued = harness.grants(reseller.config.clone()).issue_grant(request).await?;
    Ok(issued.into_owned().grant.into_option().unwrap())
}

#[tokio::test]
async fn a_moderator_applied_at_a_project_is_an_ordinary_grant_that_no_edit_widens() {
    let harness = Harness::new().await;
    let (reseller, org, project) = reseller_and_project(&harness).await;
    let grants = harness.grants(reseller.config.clone());
    let actions = ["workload.console.read", "workload.restart"];
    let create = pb::CreateGrantTemplateRequest {
        mutation: mutation("moderator").into(),
        organization: org.clone(),
        template: template("Moderator", &actions, pb::ObjectKind::OBJECT_KIND_PROJECT).into(),
        ..Default::default()
    };
    let made = grants.create_grant_template(create).await.unwrap().into_owned();
    let id = made.template.into_option().unwrap().id;

    let before = SystemTime::now();
    let grant = from(&harness, &reseller, (&id, 1), project_scope(&project)).await.unwrap();
    assert_eq!(grant.actions, actions, "the template's actions");
    assert_eq!(grant.scope.as_option(), Some(&project_scope(&project)), "on the project");
    assert_eq!(grant.delegation.as_option().unwrap().max_depth, Some(1));
    assert_eq!(grant.label, "Moderator", "named after it");
    let provenance = grant.from_template.as_option().unwrap();
    assert_eq!((provenance.template.as_str(), provenance.version), (id.as_str(), 1));
    let expires = grant.conditions.as_option().unwrap().expires_at.as_option().unwrap();
    let expires = korpis_server::api::wire::time("expires_at", expires).unwrap();
    assert!(expires >= before + Duration::from_secs(86_400 - 5), "after its lifetime");

    // Version 2 adds a read; the grant issued from version 1 holds what it held.
    let edit = pb::UpdateGrantTemplateRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("edit") }.into(),
        id: id.clone(),
        template: template("Moderator", &PASSED, pb::ObjectKind::OBJECT_KIND_PROJECT).into(),
        ..Default::default()
    };
    grants.update_grant_template(edit).await.unwrap();
    let read = pb::GetGrantRequest { id: grant.id.clone(), ..Default::default() };
    let host = harness.grants(harness.root());
    let again = host.get_grant(read).await.unwrap().into_owned().grant.into_option().unwrap();
    assert_eq!(again.actions, actions, "an edit widens nothing already issued");
    let stale = from(&harness, &reseller, (&id, 1), project_scope(&project)).await.unwrap_err();
    assert_eq!(stale.code, ErrorCode::Aborted, "issued against version 1");
    let wrong = from(&harness, &reseller, (&id, 2), org_scope(&org)).await.unwrap_err();
    assert_eq!(wrong.code, ErrorCode::InvalidArgument, "a Moderator is applied at a project");
    let issued = harness.effects("grant.issued").await;
    assert!(issued.iter().any(|(_, g)| *g == Some(id_of(&reseller.grant))), "under its grant");
}

fn id_of(grant: &str) -> Uuid {
    Id::parse(IdKind::Grant, grant).unwrap().uuid()
}

#[tokio::test]
async fn a_template_grants_nobody_what_its_issuer_cannot_pass_on() {
    let harness = Harness::new().await;
    let (reseller, org, project) = reseller_and_project(&harness).await;
    let grants = harness.grants(reseller.config.clone());
    let exec = ["workload.exec"];
    let mut body = template("Shell", &exec, pb::ObjectKind::OBJECT_KIND_PROJECT);
    body.delegation = delegation(None, &[]).into();
    let create = pb::CreateGrantTemplateRequest {
        mutation: mutation("shell").into(),
        organization: org.clone(),
        template: body.into(),
        ..Default::default()
    };
    let made = grants.create_grant_template(create).await.unwrap().into_owned();
    let id = made.template.into_option().unwrap().id;
    let refused = from(&harness, &reseller, (&id, 1), project_scope(&project)).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArgument, "the reseller holds no exec to pass on");

    // Naming actions beside a template, or issuing from a retired one, is refused as well.
    let (token, _) = harness.bare_token().await;
    let both = pb::IssueGrantRequest {
        mutation: mutation("both").into(),
        parent: reseller.grant.clone(),
        subject: principal::to_pb(&Principal::Token(token)).into(),
        actions: vec!["workload.restart".into()],
        scope: project_scope(&project).into(),
        template: pb::TemplateRef { template: id.clone(), version: 1, ..Default::default() }.into(),
        ..Default::default()
    };
    let named = grants.issue_grant(both).await.unwrap_err();
    assert_eq!(named.code, ErrorCode::InvalidArgument, "the template says the actions");
    let retire = pb::RetireGrantTemplateRequest {
        mutation: mutation("retire").into(),
        id: id.clone(),
        ..Default::default()
    };
    grants.retire_grant_template(retire).await.unwrap();
    let gone = from(&harness, &reseller, (&id, 1), project_scope(&project)).await.unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound, "a retired template is offered nowhere");
}
