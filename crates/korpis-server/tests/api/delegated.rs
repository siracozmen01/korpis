// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a member delegated, on a real control plane (§5.1 of docs/08-identity.md): a grant a
//! member of a bound set issued through it waits for the set's re-verification to confirm the
//! member, works while it does, and is refused once the provider says the member left, without
//! anyone revoking it. The grant is written here as the issue path writes one, so the member
//! need not sign in; a member signed in through their provider issuing it is `member_issue.rs`.

use crate::bindings::{bind, from_template, set};
use crate::idp::{LEFT, serving};
use crate::recipes::project;
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_app::ports::{GrantWrite, Store, Tx};
use korpis_core::authz::{
    Action, Conditions, Delegation, Grant, GrantRecord, Kind, ObjectRef, Scope, Via,
};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;
use uuid::Uuid;

/// A template letting its holder read a project and pass that on once.
pub fn readers() -> pb::GrantTemplateBody {
    pb::GrantTemplateBody {
        name: "Readers".into(),
        actions: vec!["project.read".into()],
        scope_pattern: pb::ScopePattern {
            kind: pb::ObjectKind::OBJECT_KIND_PROJECT.into(),
            ..Default::default()
        }
        .into(),
        delegation: pb::DelegationPolicy {
            allowed: true,
            max_depth: Some(1),
            actions: vec!["project.read".into()],
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// The admins set's grant on `at`, from the readers template its binding names; its id and the
/// binding's.
async fn admins_read(h: &Harness, at: &str) -> (Uuid, String) {
    let org = h.root_org();
    let grants = h.grants(h.root());
    let create = pb::CreateGrantTemplateRequest {
        mutation: mutation("readers").into(),
        organization: org.clone(),
        template: readers().into(),
        ..Default::default()
    };
    let tpl = grants.create_grant_template(create).await.unwrap().into_owned();
    let tpl = tpl.template.into_option().unwrap().id;
    let terms = pb::IdentityBindingTerms {
        template: Some(pb::TemplateRef { template: tpl.clone(), version: 1, ..Default::default() })
            .into(),
        ..Default::default()
    };
    let binding = bind(h, &org, set(&org, "admins"), terms).await.unwrap();
    verify(h, &binding.id).await;
    let issued = grants.issue_grant(from_template(h, &set(&org, "admins"), (&tpl, 1), at)).await;
    let id = issued.unwrap().into_owned().grant.into_option().unwrap().id;
    (Id::parse(IdKind::Grant, &id).unwrap().uuid(), binding.id)
}

async fn verify(h: &Harness, binding: &str) {
    let req = pb::VerifyIdentityBindingRequest { id: binding.to_owned(), ..Default::default() };
    h.identity(h.root()).verify_identity_binding(req).await.unwrap();
}

/// The grant `member` issued through the admins set to `token`, written as the issue path does.
async fn delegate(h: &Harness, (parent, at): (Uuid, &str), member: &Principal, token: Uuid) {
    let org = h.root_org();
    let set = Principal::ExternalSet {
        provider: format!("{org}/corp"),
        realm: "groups".into(),
        member_of: "admins".into(),
    };
    let holder = Principal::Token(token);
    let now = SystemTime::now();
    let project = Id::parse(IdKind::Project, at).unwrap().uuid();
    let record = GrantRecord {
        grant: Grant {
            id: Uuid::now_v7(),
            issued_at: now,
            via: Some(Via { set: set.subject(), member: member.subject() }),
            subject: holder.subject(),
            actions: BTreeSet::from([Action::parse("project.read").unwrap()]),
            scope: Scope::Object(ObjectRef::new(Kind::Project, project)),
            conditions: Conditions::default(),
            parent: Some(parent),
            delegation: Delegation::default(),
            link_form: false,
            uses: 0,
            revoked: false,
            suspended: false,
            irreversible_override: false,
            root: false,
        },
        holder,
        issued_by: member.clone(),
        issued_at: now,
        revoked_at: None,
        suspended_at: None,
        label: "a helper".into(),
        from_template: None,
    };
    let store = h.db.store().await.unwrap();
    let mut tx = store.begin().await.unwrap();
    tx.insert_grant(&record).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn what_a_member_delegated_works_only_while_the_set_still_has_them() {
    let h = Harness::new().await;
    let (_running, _) = serving(&h, 58, &Arc::new(AtomicBool::new(true))).await;
    let survival = project(&h, &h.root_org(), "survival").await;
    let (role, binding) = admins_read(&h, &survival).await;
    let carol = Principal::ExternalUser {
        provider: format!("{}/corp", h.root_org()),
        user: "carol".into(),
    };
    let (token, secret) = h.bare_token().await;
    delegate(&h, (role, &survival), &carol, token).await;
    let helper = h.projects(h.config(Some(secret.expose())));
    let get = || pb::GetProjectRequest { id: survival.clone(), ..Default::default() };
    let waiting = helper.get_project(get()).await.unwrap_err();
    assert_eq!(waiting.code, ErrorCode::NotFound, "until the set's provider confirms her");

    verify(&h, &binding).await;
    helper.get_project(get()).await.expect("she is in the set, so what she passed on works");

    LEFT.lock().unwrap().push("carol".into());
    verify(&h, &binding).await;
    let left = helper.get_project(get()).await.unwrap_err();
    assert_eq!(left.code, ErrorCode::NotFound, "she left, and nobody revoked anything");
}
