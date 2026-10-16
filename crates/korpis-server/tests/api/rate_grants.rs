// SPDX-License-Identifier: AGPL-3.0-or-later

//! The organizations a request is charged to (§10 of docs/10-api.md): those the subject holds a
//! live grant in, read over every grant it holds. A grant revoked, expired, or descending from a
//! revoked one charges nothing, so an organization that revokes a subject stops paying for it.

use crate::recipes::project;
use crate::support::{Harness, mutation, org_scope};
use connectrpc::client::ClientConfig;
use korpis_app::ports::RateBuckets as _;
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::{principal, scope};
use std::time::SystemTime;
use uuid::Uuid;

/// Issues `org.read` on `on` to the token `holder`, from `parent` as `issuer` presents, allowed
/// to issue it on if `delegates`; the grant.
async fn grant(
    h: &Harness,
    (issuer, parent): (ClientConfig, String),
    (holder, on): (Uuid, pb::Scope),
    key: &str,
    delegates: bool,
) -> String {
    let request = pb::IssueGrantRequest {
        mutation: mutation(key).into(),
        parent,
        subject: principal::to_pb(&Principal::Token(holder)).into(),
        actions: vec!["org.read".into()],
        scope: on.into(),
        delegation: pb::DelegationPolicy {
            allowed: delegates,
            max_depth: Some(u32::from(delegates)),
            actions: vec!["org.read".into()],
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    let answer = h.grants(issuer).issue_grant(request).await.unwrap().into_owned();
    answer.grant.as_option().unwrap().id.clone()
}

async fn revoke(h: &Harness, grant: String, key: &str) {
    let revoke =
        pb::RevokeGrantRequest { mutation: mutation(key).into(), grant, ..Default::default() };
    h.grants(h.root()).revoke_grant(revoke).await.unwrap();
}

/// A new organization beneath the root, named `name`.
async fn organization(h: &Harness, name: &str) -> String {
    let create = pb::CreateOrganizationRequest {
        mutation: mutation(name).into(),
        name: name.into(),
        parent: Some(h.root_org()),
        ..Default::default()
    };
    let created = h.orgs(h.root()).create_organization(create).await.unwrap().into_owned();
    created.organization.as_option().unwrap().id.clone()
}

fn uuid(kind: IdKind, id: &str) -> Uuid {
    Id::parse(kind, id).unwrap().uuid()
}

#[tokio::test]
async fn only_the_organizations_of_live_grants_are_charged() {
    let h = Harness::new().await;
    let (kept, gone, cut, lapsed) = (
        organization(&h, "kept").await,
        organization(&h, "gone").await,
        organization(&h, "cut").await,
        organization(&h, "lapsed").await,
    );
    let web = uuid(IdKind::Project, &project(&h, &kept, "web").await);
    let (holder, _) = h.bare_token().await;
    let root = || (h.root(), h.root_grant());

    let in_project = scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Project, web)));
    grant(&h, root(), (holder, in_project), "g1", false).await;
    let revoked = grant(&h, root(), (holder, org_scope(&gone)), "g2", false).await;
    revoke(&h, revoked, "r1").await;
    let (issuer, secret) = h.bare_token().await;
    let parent = grant(&h, root(), (issuer, org_scope(&cut)), "g3", true).await;
    let from_issuer = (h.config(Some(secret.expose())), parent.clone());
    grant(&h, from_issuer, (holder, org_scope(&cut)), "g4", false).await;
    revoke(&h, parent, "r2").await;
    let expired = grant(&h, root(), (holder, org_scope(&lapsed)), "g5", false).await;
    let store = h.db.store().await.unwrap();
    sqlx::query(
        "UPDATE grants SET conditions = conditions || '{\"expires_at\": \"2020-01-01T00:00:00Z\"}'
          WHERE id = $1",
    )
    .bind(uuid(IdKind::Grant, &expired))
    .execute(store.pool())
    .await
    .unwrap();

    let key = Principal::Token(holder).key();
    let held = store.organizations_held(&key, SystemTime::now()).await.unwrap();
    assert_eq!(held, vec![uuid(IdKind::Organization, &kept)], "only the project's organization");
}
