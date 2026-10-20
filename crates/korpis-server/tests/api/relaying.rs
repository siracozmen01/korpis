// SPDX-License-Identifier: AGPL-3.0-or-later

//! Acting for an asserted person (§5.2 of docs/08-identity.md), against a real control plane: a
//! relay that was handed a ticket makes the ordinary call with its own credential and the
//! ticket, and the gateway acts as the person, through the sets the assertion names, for the
//! one action on the one object the ticket names, once. The effect names the person and the
//! relay; anything else asked with the ticket is refused, and no ticket issues a grant.

use crate::bindings::{bind, issue, set};
use crate::idp::serving;
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use connectrpc::client::ClientConfig;
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// A ticket for `action` on the root organization, as the person presenting `evidence`.
async fn ticket(h: &Harness, relay: &str, evidence: &str, action: &str) -> String {
    let org = (pb::ObjectKind::OBJECT_KIND_ORGANIZATION, h.root_org());
    ticket_for(h, relay, evidence, action, org).await
}

/// A ticket for `action` on `object`, as the person presenting `evidence`.
async fn ticket_for(
    h: &Harness,
    relay: &str,
    evidence: &str,
    action: &str,
    (kind, id): (pb::ObjectKind, String),
) -> String {
    let object = pb::ObjectRef { kind: kind.into(), id, ..Default::default() };
    let req = pb::AssertIdentityRequest {
        organization: h.root_org(),
        provider: "corp".into(),
        evidence: evidence.as_bytes().to_vec(),
        action: action.into(),
        object: Some(object).into(),
        ..Default::default()
    };
    let asserted = h.identity(h.config(Some(relay))).assert_identity(req).await.unwrap();
    asserted.into_owned().ticket
}

/// The relay's client, acting for the person `ticket` asserted.
fn acting(h: &Harness, relay: &str, ticket: &str) -> ClientConfig {
    h.config(Some(relay)).with_default_header("korpis-as", ticket.to_owned())
}

fn create(name: &str, org: &str) -> pb::CreateProjectRequest {
    pb::CreateProjectRequest {
        mutation: mutation(&format!("relayed-{name}")).into(),
        organization: org.to_owned(),
        name: name.into(),
        ..Default::default()
    }
}

/// The admins set, bound, verified, and holding `project.create` on the root organization.
async fn admins_may_create(h: &Harness) {
    admins_hold(h, "project.create").await;
}

/// The admins set, bound, verified, and holding `action` on the root organization.
async fn admins_hold(h: &Harness, action: &str) {
    let org = h.root_org();
    let made = bind(h, &org, set(&org, "admins"), pb::IdentityBindingTerms::default());
    let made = made.await.unwrap();
    let verify = pb::VerifyIdentityBindingRequest { id: made.id, ..Default::default() };
    h.identity(h.root()).verify_identity_binding(verify).await.unwrap();
    issue(h, &set(&org, "admins"), &[action], org_scope(&org), false).await.unwrap();
}

/// Who did `action` as an external identity, and the relay each did it through.
async fn actors(h: &Harness, action: &str) -> Vec<(String, Option<String>)> {
    let store = h.db.store().await.unwrap();
    let external = "SELECT actor, relay FROM effect WHERE action = $1 AND actor LIKE 'ext:%' \
                    ORDER BY at, id";
    sqlx::query_as(external).bind(action).fetch_all(store.pool()).await.unwrap()
}

#[tokio::test]
async fn a_person_acts_once_through_their_set_and_the_effect_names_the_relay() {
    let h = Harness::new().await;
    let (_running, relay) = serving(&h, 55, &Arc::new(AtomicBool::new(true))).await;
    admins_may_create(&h).await;
    let org = h.root_org();
    let t = ticket(&h, &relay, "ann:c1", "project.create").await;
    let as_ann = h.projects(acting(&h, &relay, &t));
    as_ann.create_project(create("by-ann", &org)).await.expect("ann is an admin");
    let created = actors(&h, "project.create").await;
    assert_eq!(created.len(), 1, "{created:?}");
    assert!(created[0].0.ends_with("/corp:user:ann"), "the person acted: {created:?}");
    assert!(created[0].1.as_deref().is_some_and(|r| r.starts_with("extension:")), "and the relay");

    let again = as_ann.create_project(create("again", &org)).await.unwrap_err();
    assert_eq!(again.code, ErrorCode::Unauthenticated, "a ticket is spent once");
    let plain = h.projects(h.config(Some(&relay))).create_project(create("plain", &org)).await;
    assert!(plain.is_err(), "the relay alone holds nothing of the person's");
}

#[tokio::test]
async fn a_ticket_permits_only_its_action_and_never_a_grant() {
    let h = Harness::new().await;
    let (_running, relay) = serving(&h, 56, &Arc::new(AtomicBool::new(true))).await;
    admins_may_create(&h).await;
    let org = h.root_org();
    let t = ticket(&h, &relay, "ann:o1", "project.create").await;
    let other = pb::CreateOrganizationRequest {
        mutation: mutation("relayed-org").into(),
        name: "elsewhere".into(),
        parent: Some(org.clone()),
        ..Default::default()
    };
    let e = h.orgs(acting(&h, &relay, &t)).create_organization(other).await.unwrap_err();
    assert!(matches!(e.code, ErrorCode::NotFound | ErrorCode::PermissionDenied), "{e:?}");
    let refused = actors(&h, "org.create").await;
    assert_eq!(refused.len(), 1, "the refusal is recorded: {refused:?}");
    assert!(refused[0].1.is_some(), "naming the relay");

    let t = ticket(&h, &relay, "ann:g1", "project.create").await;
    let grant = pb::IssueGrantRequest {
        mutation: mutation("relayed-grant").into(),
        parent: h.root_grant(),
        subject: principal::to_pb(&Principal::User(uuid::Uuid::now_v7())).into(),
        actions: vec!["project.create".into()],
        scope: org_scope(&org).into(),
        ..Default::default()
    };
    let e = h.grants(acting(&h, &relay, &t)).issue_grant(grant).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "no ticket issues a grant: {e:?}");
}

#[tokio::test]
async fn a_person_revokes_the_one_grant_their_ticket_names_through_their_set() {
    let h = Harness::new().await;
    let (_running, relay) = serving(&h, 57, &Arc::new(AtomicBool::new(true))).await;
    admins_hold(&h, "grant.revoke").await;
    let (token, _) = h.bare_token().await;
    let leaked = h.issue_to(token, "leaked", &["org.read"]).await;
    let kept = h.issue_to(token, "kept", &["org.read"]).await;
    let revoke = |grant: &str, key: &str| pb::RevokeGrantRequest {
        mutation: mutation(key).into(),
        grant: grant.to_owned(),
        ..Default::default()
    };
    let named = (pb::ObjectKind::OBJECT_KIND_GRANT, leaked.clone());
    let t = ticket_for(&h, &relay, "ann:r1", "grant.revoke", named.clone()).await;
    let other = h.grants(acting(&h, &relay, &t)).revoke_grant(revoke(&kept, "r-kept")).await;
    assert_eq!(other.unwrap_err().code, ErrorCode::PermissionDenied, "not the grant it names");

    let t = ticket_for(&h, &relay, "ann:r2", "grant.revoke", named).await;
    let done = h.grants(acting(&h, &relay, &t)).revoke_grant(revoke(&leaked, "r-leaked")).await;
    assert!(done.is_ok(), "through the admins' grant.revoke: {done:?}");
    let revoked = actors(&h, "grant.revoke").await;
    assert_eq!(revoked.len(), 1, "{revoked:?}");
    assert!(revoked[0].1.is_some(), "naming the relay");
}
