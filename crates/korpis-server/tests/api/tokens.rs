// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tokens issued to hold one grant (§3.7 of docs/08-identity.md): the secret is answered once,
//! the token authenticates online so revoking its grant takes effect at once, and a grant with
//! no parent named is attenuated from the one grant the caller holds that it is valid from.

use crate::support::{Harness, detail, mutation, org_scope};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;

fn to_new_token(key: &str, org: &str, label: &str) -> pb::IssueGrantRequest {
    pb::IssueGrantRequest {
        mutation: mutation(key).into(),
        actions: vec!["org.read".into()],
        scope: org_scope(org).into(),
        label: label.into(),
        to_new_token: true,
        ..Default::default()
    }
}

async fn orgs_seen(h: &Harness, secret: &str) -> usize {
    let listed = h
        .orgs(h.config(Some(secret)))
        .list_organizations(pb::ListOrganizationsRequest::default())
        .await;
    listed.map_or(0, |r| r.into_owned().organizations.len())
}

#[tokio::test]
async fn a_new_token_holds_its_grant_until_the_grant_is_revoked() {
    let h = Harness::new().await;
    let org = h.root_org();
    let request = to_new_token("ci", &org, "github actions · servers");
    let issued = h.grants(h.root()).issue_grant(request.clone()).await.unwrap().into_owned();
    let grant = issued.grant.as_option().unwrap().clone();
    assert!(issued.token.starts_with("korpis_"), "the secret, once");
    assert_eq!(grant.label, "github actions · servers");
    assert_eq!(grant.parent.as_deref(), Some(h.root_grant().as_str()), "the one grant held");
    let replayed = h.grants(h.root()).issue_grant(request).await.unwrap().into_owned();
    assert_eq!(replayed.grant.as_option().unwrap().id, grant.id);
    assert!(replayed.token.is_empty(), "a replay never repeats the secret");

    assert_eq!(orgs_seen(&h, &issued.token).await, 1);
    let revoke = pb::RevokeGrantRequest {
        mutation: mutation("revoke").into(),
        grant: grant.id.clone(),
        ..Default::default()
    };
    h.grants(h.root()).revoke_grant(revoke).await.unwrap();
    assert_eq!(orgs_seen(&h, &issued.token).await, 0, "revoked at the next request");
}

#[tokio::test]
async fn a_new_token_is_named_and_a_parent_is_never_guessed() {
    let h = Harness::new().await;
    let org = h.root_org();
    let e = h.grants(h.root()).issue_grant(to_new_token("a", &org, " ")).await.unwrap_err();
    assert_eq!(detail(&e).field, "label");
    let (token, secret) = h.bare_token().await;
    let mut both = to_new_token("b", &org, "x");
    both.subject = principal::to_pb(&Principal::Token(token)).into();
    let e = h.grants(h.root()).issue_grant(both).await.unwrap_err();
    assert_eq!(detail(&e).field, "subject");

    for key in ["one", "two"] {
        let delegating = pb::IssueGrantRequest {
            mutation: mutation(key).into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["org.read".into()],
            scope: org_scope(&org).into(),
            delegation: pb::DelegationPolicy {
                allowed: true,
                max_depth: Some(1),
                actions: vec!["org.read".into()],
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        h.grants(h.root()).issue_grant(delegating).await.unwrap();
    }
    let holder = h.config(Some(secret.expose()));
    let e = h.grants(holder.clone()).issue_grant(to_new_token("c", &org, "x")).await.unwrap_err();
    assert_eq!(detail(&e).field, "parent", "two grants could parent it: {}", detail(&e).message);
    let mut named = to_new_token("d", &org, "x");
    let held = h.grants(holder.clone()).list_grants(pb::ListGrantsRequest::default()).await;
    named.parent = held.unwrap().into_owned().grants[0].id.clone();
    h.grants(holder).issue_grant(named).await.unwrap();
}
