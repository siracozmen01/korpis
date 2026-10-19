// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings (§5 of docs/08-identity.md), on a real control plane: an operator declares
//! that an identity provider installed into the organization vouches for a set, and a grant to
//! the set is then issued like any other, from the template its binding names. A grant names an
//! external identity only through a live binding, a provider vouches nowhere above where it is
//! installed, and whether a set's grants may carry irreversible actions is the binding's to say.

use crate::extensions::{DNS, install, push_document};
use crate::recipes::project;
use crate::support::{Harness, detail, mutation, org_scope};
use connectrpc::{ConnectError, ErrorCode};
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::{external_identity, subject};
use korpis_server::api::wire::scope;

/// An identity provider named `corp`: the DNS document serving `identity` instead.
pub fn identity_document() -> String {
    DNS.replace("{type: dns, name: cloudflare}", "{type: identity, name: corp}")
        .replace("provider.dns, ", "provider.identity, ")
}

/// Installs the identity provider into `org`, its workload in a project there.
async fn provider_in(h: &Harness, org: &str, n: u8) {
    let repo = push_document(&identity_document()).await;
    let project = project(h, org, &format!("idp-{n}")).await;
    let here = |key: &str| pb::InstallExtensionRequest {
        organization: org.to_owned(),
        ..install(h, key, &repo, &project)
    };
    let extensions = h.extensions(h.root());
    let dry = pb::InstallExtensionRequest { dry_run: true, ..here("dry") };
    let shown = extensions.install_extension(dry).await.unwrap().into_owned();
    let approved = shown.extension.into_option().unwrap().manifest.into_option().unwrap().needs;
    let req = pb::InstallExtensionRequest { approved_needs: approved, ..here(&format!("idp-{n}")) };
    extensions.install_extension(req).await.unwrap();
}

pub fn set(org: &str, group: &str) -> pb::ExternalIdentity {
    let s =
        pb::ExternalSet { realm: "groups".into(), member_of: group.into(), ..Default::default() };
    pb::ExternalIdentity {
        provider: format!("{org}/corp"),
        selector: Some(external_identity::Selector::Set(Box::new(s))),
        ..Default::default()
    }
}

pub async fn bind(
    h: &Harness,
    org: &str,
    subject: pb::ExternalIdentity,
    terms: pb::IdentityBindingTerms,
) -> Result<pb::IdentityBinding, ConnectError> {
    let req = pb::CreateIdentityBindingRequest {
        mutation: mutation(&format!("bind-{}", uuid::Uuid::now_v7())).into(),
        organization: org.to_owned(),
        provider: "corp".into(),
        subject: subject.into(),
        terms: terms.into(),
        ..Default::default()
    };
    let made = h.identity(h.root()).create_identity_binding(req).await?.into_owned();
    Ok(made.binding.into_option().unwrap())
}

fn project_scope(id: &str) -> pb::Scope {
    let id = Id::parse(IdKind::Project, id).unwrap().uuid();
    scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Project, id)))
}

/// Issues `actions` on `at` to `to`, delegating them when `delegates`.
async fn issue(
    h: &Harness,
    to: &pb::ExternalIdentity,
    actions: &[&str],
    at: pb::Scope,
    delegates: bool,
) -> Result<pb::Grant, ConnectError> {
    let req = pb::IssueGrantRequest {
        mutation: mutation(&format!("issue-{}", uuid::Uuid::now_v7())).into(),
        parent: h.root_grant(),
        subject: pb::Subject {
            kind: Some(subject::Kind::External(Box::new(to.clone()))),
            ..Default::default()
        }
        .into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope: at.into(),
        delegation: pb::DelegationPolicy {
            allowed: delegates,
            max_depth: delegates.then_some(1),
            actions: if delegates { vec![actions[0].to_owned()] } else { Vec::new() },
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    let issued = h.grants(h.root()).issue_grant(req).await?.into_owned();
    Ok(issued.grant.into_option().unwrap())
}

fn refused(e: &ConnectError, field: &str) {
    assert_eq!(e.code, ErrorCode::InvalidArgument, "{e:?}");
    assert_eq!(detail(e).field, field, "{e:?}");
}

#[tokio::test]
async fn a_set_is_bound_where_its_provider_is_installed_and_named_only_while_bound() {
    let h = Harness::new().await;
    let org = h.root_org();
    provider_in(&h, &org, 1).await;
    let admins = set(&org, "admins");
    let made = bind(&h, &org, admins.clone(), pb::IdentityBindingTerms::default()).await.unwrap();
    assert_eq!(made.kind.as_known(), Some(pb::BindingKind::BINDING_KIND_SET));
    assert_eq!(made.state.as_known(), Some(pb::BindingState::BINDING_STATE_UNVERIFIED));
    assert!(made.last_verified_at.as_option().is_none(), "nothing has confirmed it yet");
    assert_eq!((made.version, made.organization.as_str()), (1, org.as_str()));
    let again = bind(&h, &org, admins.clone(), pb::IdentityBindingTerms::default()).await;
    assert_eq!(again.unwrap_err().code, ErrorCode::Aborted, "a subject is bound once");
    let mut elsewhere = set(&org, "admins");
    elsewhere.provider = format!("{org}/other");
    let wrong = bind(&h, &org, elsewhere, pb::IdentityBindingTerms::default()).await;
    refused(&wrong.unwrap_err(), "subject");

    let survival = project(&h, &org, "survival").await;
    let at = || project_scope(&survival);
    issue(&h, &admins, &["workload.restart"], at(), false).await.unwrap();
    let delegating = issue(&h, &admins, &["workload.restart"], at(), true).await.unwrap_err();
    refused(&delegating, "delegation");
    let unbound = issue(&h, &set(&org, "guests"), &["workload.restart"], at(), false).await;
    refused(&unbound.unwrap_err(), "subject");
    let deleting = issue(&h, &admins, &["workload.delete"], at(), false).await;
    assert_eq!(deleting.unwrap_err().code, ErrorCode::InvalidArgument, "irreversible by default");

    let identity = h.identity(h.root());
    let edit = pb::UpdateIdentityBindingRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("allow") }.into(),
        id: made.id.clone(),
        terms: pb::IdentityBindingTerms { allows_irreversible: true, ..Default::default() }.into(),
        ..Default::default()
    };
    let edited = identity.update_identity_binding(edit).await.unwrap().into_owned();
    assert_eq!(edited.binding.as_option().unwrap().version, 2);
    issue(&h, &admins, &["workload.delete"], at(), false).await.unwrap();

    let listed = identity
        .list_identity_bindings(pb::ListIdentityBindingsRequest {
            organization: org.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(listed.bindings.len(), 1);
    let retire = pb::RetireIdentityBindingRequest {
        mutation: mutation("retire").into(),
        id: made.id.clone(),
        ..Default::default()
    };
    identity.retire_identity_binding(retire).await.unwrap();
    let gone = issue(&h, &admins, &["workload.restart"], at(), false).await.unwrap_err();
    refused(&gone, "subject");
    bind(&h, &org, admins, pb::IdentityBindingTerms::default()).await.unwrap();
}

#[tokio::test]
async fn a_provider_vouches_beneath_where_it_is_installed_and_nowhere_else() {
    let h = Harness::new().await;
    let root = h.root_org();
    let made = h
        .orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation("customer").into(),
            parent: Some(root.clone()),
            name: "customer".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let customer = made.organization.into_option().unwrap().id;
    let none = bind(&h, &customer, set(&customer, "admins"), pb::IdentityBindingTerms::default());
    refused(&none.await.unwrap_err(), "provider");
    provider_in(&h, &customer, 2).await;
    let admins = set(&customer, "admins");
    bind(&h, &customer, admins.clone(), pb::IdentityBindingTerms::default()).await.unwrap();
    let theirs = project(&h, &customer, "theirs").await;
    issue(&h, &admins, &["workload.restart"], project_scope(&theirs), false).await.unwrap();
    let above = issue(&h, &admins, &["workload.restart"], org_scope(&root), false).await;
    refused(&above.unwrap_err(), "scope");
}

/// A Moderator template allowing `actions` on a project, delegating restart one level down.
fn moderator(actions: &[&str]) -> pb::GrantTemplateBody {
    pb::GrantTemplateBody {
        name: "Moderator".into(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        scope_pattern: pb::ScopePattern {
            kind: pb::ObjectKind::OBJECT_KIND_PROJECT.into(),
            ..Default::default()
        }
        .into(),
        delegation: pb::DelegationPolicy {
            allowed: true,
            max_depth: Some(1),
            actions: vec!["workload.restart".into()],
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// Issuing from `template` at `version` on project `at` to `to`.
fn from_template(
    h: &Harness,
    to: &pb::ExternalIdentity,
    (template, version): (&str, u64),
    at: &str,
) -> pb::IssueGrantRequest {
    pb::IssueGrantRequest {
        mutation: mutation(&format!("from-{}", uuid::Uuid::now_v7())).into(),
        parent: h.root_grant(),
        subject: pb::Subject {
            kind: Some(subject::Kind::External(Box::new(to.clone()))),
            ..Default::default()
        }
        .into(),
        scope: project_scope(at).into(),
        template: pb::TemplateRef { template: template.into(), version, ..Default::default() }
            .into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_set_receives_the_template_its_binding_names_at_that_version() {
    let h = Harness::new().await;
    let org = h.root_org();
    provider_in(&h, &org, 3).await;
    let grants = h.grants(h.root());
    let create = pb::CreateGrantTemplateRequest {
        mutation: mutation("moderator").into(),
        organization: org.clone(),
        template: moderator(&["workload.restart"]).into(),
        ..Default::default()
    };
    let made = grants.create_grant_template(create).await.unwrap().into_owned();
    let tpl = made.template.into_option().unwrap().id;
    let r = |version| pb::TemplateRef { template: tpl.clone(), version, ..Default::default() };
    let terms = |version| pb::IdentityBindingTerms {
        template: Some(r(version)).into(),
        ..Default::default()
    };
    let mods = set(&org, "moderators");
    let binding = bind(&h, &org, mods.clone(), terms(1)).await.unwrap();
    let survival = project(&h, &org, "survival").await;
    let by_hand = issue(&h, &mods, &["workload.restart"], project_scope(&survival), false).await;
    refused(&by_hand.unwrap_err(), "template");
    let from = |version| from_template(&h, &mods, (&tpl, version), &survival);
    let issued = grants.issue_grant(from(1)).await.unwrap().into_owned();
    let delegation = issued.grant.as_option().unwrap().delegation.as_option().unwrap().clone();
    assert!(delegation.allowed, "a set delegates as its binding's template says");

    let edit = pb::UpdateGrantTemplateRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("edit") }.into(),
        id: tpl.clone(),
        template: moderator(&["workload.restart", "project.read"]).into(),
        ..Default::default()
    };
    grants.update_grant_template(edit).await.unwrap();
    refused(&grants.issue_grant(from(2)).await.unwrap_err(), "template");
    let moved = pb::UpdateIdentityBindingRequest {
        mutation: pb::Mutation { expected_version: Some(1), ..mutation("move") }.into(),
        id: binding.id,
        terms: terms(2).into(),
        ..Default::default()
    };
    h.identity(h.root()).update_identity_binding(moved).await.unwrap();
    grants.issue_grant(from(2)).await.unwrap();
}
