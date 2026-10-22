// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in at a portal (§9.8 of docs/08-identity.md), over the portal's own connection: an
//! invitation redeemed there registers a passkey for the portal's relying party, whose challenge
//! is answered there and nowhere else; the session it begins acts only beneath the portal's
//! organization, as every access token it is exchanged for does, whatever the account's grants
//! reach, and what it issues is held there too.

use crate::domains::child;
use crate::panel::accounts;
use crate::portal_client::{answered_at, portal_of, redeemed_at, signed_in_at};
use crate::refusal::reason;
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::webauthn::testing::SoftAuthenticator;
use korpis_proto::korpis::v1 as pb;

const NAME: &str = "panel.kestrel.example";

/// Issues `org.read` on the root organization to a new user; the invitation, and the grant.
async fn invited(h: &Harness) -> (String, String) {
    let issued = pb::IssueGrantRequest {
        mutation: mutation("invite").into(),
        actions: vec!["org.read".to_owned()],
        scope: org_scope(&h.root_org()).into(),
        to_new_user: pb::NewUser { display_name: "Dana".into(), ..Default::default() }.into(),
        ..Default::default()
    };
    let issued = h.grants(h.root()).issue_grant(issued).await.unwrap().into_owned();
    (issued.invitation, issued.grant.into_option().unwrap().id)
}

#[tokio::test]
async fn a_portal_s_passkey_signs_in_there_alone_and_its_session_acts_beneath_its_organization() {
    let h = Harness::new().await;
    let reseller = child(&h, "kestrel").await;
    let portal = portal_of(&h, ("p", &reseller), ("kestrel.example", NAME)).await;
    let (invitation, grant) = invited(&h).await;
    let mut browser = SoftAuthenticator::new(NAME, &portal.origin);
    redeemed_at(&portal, &invitation, &browser).await;

    // A challenge issued at the portal is not answered at the installation's own address.
    let elsewhere = answered_at(&portal, &mut browser).await;
    let refused = accounts(&h).sign_in_with_passkey(elsewhere).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unauthenticated);

    let (_, access) = signed_in_at(&portal, &mut browser).await;

    // The token reaches the portal's organization, wherever it is presented, and nothing above.
    let confined = h.config(Some(&access));
    let get = |id: &str| pb::GetOrganizationRequest { id: id.into(), ..Default::default() };
    h.orgs(confined.clone()).get_organization(get(&reseller)).await.unwrap();
    let above = h.orgs(confined.clone()).get_organization(get(&h.root_org())).await.unwrap_err();
    assert_eq!(above.code, ErrorCode::NotFound, "the root the account's grant is on");
    let listed = pb::ListOrganizationsRequest { parent: Some(h.root_org()), ..Default::default() };
    let listed = h.orgs(confined.clone()).list_organizations(listed).await;
    assert_eq!(listed.unwrap_err().code, ErrorCode::NotFound, "nor what lies beneath the root");

    // What it issues is held beneath the organization too.
    let token = pb::IssueGrantRequest {
        mutation: mutation("token").into(),
        actions: vec!["org.read".to_owned()],
        scope: org_scope(&h.root_org()).into(),
        to_new_token: true,
        label: "pipeline".into(),
        ..Default::default()
    };
    let token = h.grants(confined.clone()).issue_grant(token).await.unwrap_err();
    assert_eq!(reason(&token), "session.portal_confined");
    let revoke =
        pb::RevokeGrantRequest { mutation: mutation("revoke").into(), grant, ..Default::default() };
    let revoked = h.grants(confined).revoke_grant(revoke).await.unwrap_err();
    assert_eq!(reason(&revoked), "session.portal_confined", "nor its own grant above it");

    // A `korpis login` approved from the portal's session is confined as it is (Finding 129).
    let cli = h.config(Some(&cli_approved_by(&h, &access).await));
    let above = h.orgs(cli.clone()).get_organization(get(&h.root_org())).await.unwrap_err();
    assert_eq!(above.code, ErrorCode::NotFound);
    h.orgs(cli).get_organization(get(&reseller)).await.unwrap();
}

/// A CLI's sign-in begun at the installation's address and approved with `access`; an access
/// token its session gives.
async fn cli_approved_by(h: &Harness, access: &str) -> String {
    let sessions = |b: Option<&str>| pb::SessionServiceClient::new(h.http.clone(), h.config(b));
    let begun = sessions(None).authorize_device(pb::AuthorizeDeviceRequest::default()).await;
    let begun = begun.unwrap().into_owned();
    let approve = pb::ApproveDeviceRequest { user_code: begun.user_code, ..Default::default() };
    sessions(Some(access)).approve_device(approve).await.unwrap();
    let polled = pb::DeviceTokenRequest { device_code: begun.device_code, ..Default::default() };
    let cli = sessions(None).device_token(polled).await.unwrap().into_owned();
    let request = pb::ExchangeRequest { refresh: cli.refresh, ..Default::default() };
    sessions(None).exchange(request).await.unwrap().into_owned().access_token
}
