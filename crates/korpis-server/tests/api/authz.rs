// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grants: issuing, refusing, revoking, and minting tokens.

use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;

#[tokio::test]
async fn existence_is_gated_by_read_and_a_readable_refusal_is_denied_and_recorded() {
    let h = Harness::new().await;
    let root = h.root_org();
    let (token, secret) = h.bare_token().await;
    let reader = h.orgs(h.config(Some(secret.expose())));
    let get = || pb::GetOrganizationRequest { id: root.clone(), ..Default::default() };

    let e = reader.get_organization(get()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "without a grant the organization does not exist");

    h.issue_to(token, "g1", &["org.read"]).await;
    reader.get_organization(get()).await.expect("read is granted");
    let e = reader
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation("w1").into(),
            name: "nope".into(),
            parent: Some(root.clone()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "readable, so DENIED rather than NOT_FOUND");
    assert!(h.effects("org.create").await.iter().any(|(o, g)| o == "denied" && g.is_none()));
}

#[tokio::test]
async fn a_holder_cannot_widen_what_it_was_given() {
    let h = Harness::new().await;
    let (token, secret) = h.bare_token().await;
    let grant = h.issue_to(token, "g1", &["org.read"]).await;
    let e = h
        .grants(h.config(Some(secret.expose())))
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("g2").into(),
            parent: grant,
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["org.create".into()],
            scope: org_scope(&h.root_org()).into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn revocation_takes_effect_at_the_next_request_and_is_recorded() {
    let h = Harness::new().await;
    let (token, secret) = h.bare_token().await;
    let grant = h.issue_to(token, "g1", &["org.read"]).await;
    let revoked = h
        .grants(h.root())
        .revoke_grant(pb::RevokeGrantRequest {
            mutation: mutation("r1").into(),
            grant,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(
        revoked.grant.as_option().unwrap().state.as_known(),
        Some(pb::GrantState::GRANT_STATE_REVOKED)
    );
    let e = h
        .orgs(h.config(Some(secret.expose())))
        .get_organization(pb::GetOrganizationRequest { id: h.root_org(), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let revocations = h.effects("grant.revoke").await;
    assert!(
        revocations.iter().any(|(o, g)| o == "succeeded" && *g == Some(h.installed.root_grant))
    );
}

#[tokio::test]
async fn tokens_are_minted_within_the_holders_authority() {
    let h = Harness::new().await;
    let root = h.root_org();
    let grants = h.grants(h.root());
    let minted = grants
        .mint_token(pb::MintTokenRequest {
            grant: h.root_grant(),
            actions: vec!["org.read".into()],
            scope: org_scope(&root).into(),
            ttl_seconds: 3600,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert!(!minted.token.is_empty());

    let (_, secret) = h.bare_token().await;
    let e = h
        .grants(h.config(Some(secret.expose())))
        .mint_token(pb::MintTokenRequest {
            grant: h.root_grant(),
            actions: vec!["org.read".into()],
            scope: org_scope(&root).into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_token_minted_from_a_named_grant_uses_only_that_grant() {
    let h = Harness::new().await;
    let root = h.root_org();
    let (token, secret) = h.bare_token().await;
    let read_grant = h.issue_to(token, "g1", &["org.read"]).await;
    let holder = h.grants(h.config(Some(secret.expose())));
    let mint = |grant: String, action: &str| pb::MintTokenRequest {
        grant,
        actions: vec![action.into()],
        scope: org_scope(&root).into(),
        ..Default::default()
    };
    holder.mint_token(mint(read_grant.clone(), "org.read")).await.expect("held and permitted");
    let e = holder.mint_token(mint(read_grant, "org.create")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "the grant does not carry org.create");
    let e = holder.mint_token(mint(h.root_grant(), "org.read")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "the root grant is not the caller's");
}
