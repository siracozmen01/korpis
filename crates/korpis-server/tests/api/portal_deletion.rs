// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a confined session sees of its account, and what deleting a portal ends (§9.8 of
//! docs/08-identity.md): a session begun at a portal sees the sessions begun there and the
//! passkeys registered there, and one confined at the installation's own address sees neither;
//! `GetPortal` says how many passkeys are registered at the portal and how many accounts sign in
//! nowhere else; and deleting it ends the sessions begun there and removes its passkeys, each an
//! `account.factor_removed` by the system.

use crate::domains::child;
use crate::panel::{access, accounts, refresh_cookie};
use crate::portal_client::{Portal, portal_of, redeemed_at, signed_in_at};
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_core::webauthn::testing::SoftAuthenticator;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::sign_in_with_password_request::Second;

const NAME: &str = "panel.pinecone.example";
const PASSWORD: &str = "correct horse battery";

fn profile(h: &Harness, bearer: &str) -> pb::ProfileServiceClient<HttpClient> {
    pb::ProfileServiceClient::new(h.http.clone(), h.config(Some(bearer)))
}

fn sessions(h: &Harness, bearer: Option<&str>) -> pb::SessionServiceClient<HttpClient> {
    pb::SessionServiceClient::new(h.http.clone(), h.config(bearer))
}

/// What `GetPortal` says deleting `portal` would remove.
async fn reach(h: &Harness, portal: &Portal) -> (u64, u64) {
    let asked = pb::GetPortalRequest { id: portal.id.clone(), ..Default::default() };
    let got = h.portals(h.root()).get_portal(asked).await.unwrap().into_owned();
    (got.passkeys, got.accounts_only_here)
}

/// How many sessions, and passkeys, the session `bearer` gives an access token of sees.
async fn seen(h: &Harness, bearer: &str) -> (usize, usize) {
    let client = sessions(h, Some(bearer));
    let listed = client.list_sessions(pb::ListSessionsRequest::default()).await.unwrap();
    let listed = listed.into_owned().sessions.len();
    let own = profile(h, bearer).get_profile(pb::GetProfileRequest::default()).await.unwrap();
    (listed, own.into_owned().passkeys.len())
}

/// Issues `org.read` on `org` to a new user; the invitation.
async fn invited(h: &Harness, org: &str) -> String {
    let issued = pb::IssueGrantRequest {
        mutation: mutation("invite").into(),
        actions: vec!["org.read".to_owned()],
        scope: org_scope(org).into(),
        to_new_user: pb::NewUser { display_name: "Dana".into(), ..Default::default() }.into(),
        ..Default::default()
    };
    h.grants(h.root()).issue_grant(issued).await.unwrap().into_owned().invitation
}

/// A session begun at the installation's own address with `PASSWORD` and `code`; an access token
/// it gives.
async fn signed_in_elsewhere(h: &Harness, code: &str) -> String {
    let sign_in = pb::SignInWithPasswordRequest {
        name: "dana".into(),
        password: PASSWORD.into(),
        second: Some(Second::RecoveryCode(code.into())),
        ..Default::default()
    };
    let signed_in = accounts(h).sign_in_with_password(sign_in).await.unwrap();
    access(h, &refresh_cookie(signed_in.headers()).unwrap()).await
}

#[tokio::test]
async fn deleting_a_portal_ends_what_began_there_and_a_confined_session_sees_its_own_alone() {
    let h = Harness::new().await;
    let reseller = child(&h, "pinecone").await;
    let portal = portal_of(&h, ("p", &reseller), ("pinecone.example", NAME)).await;
    let mut browser = SoftAuthenticator::new(NAME, &portal.origin);
    redeemed_at(&portal, &invited(&h, &reseller).await, &browser).await;
    assert_eq!(reach(&h, &portal).await, (1, 1), "a passkey there is all it signs in with");

    // Every grant lies beneath Pinecone, so the portal's session gives the account a password.
    let (cookie, at_portal) = signed_in_at(&portal, &mut browser).await;
    let me = profile(&h, &at_portal);
    me.set_password(pb::SetPasswordRequest { password: PASSWORD.into(), ..Default::default() })
        .await
        .unwrap();
    let codes = me.renew_recovery_codes(pb::RenewRecoveryCodesRequest::default()).await;
    let codes = codes.unwrap().into_owned().recovery_codes;
    assert_eq!(reach(&h, &portal).await, (1, 0));
    let elsewhere = signed_in_elsewhere(&h, &codes[0]).await;

    // The portal's sessions see its two sessions and its passkey; the one the password began at
    // the installation's own address, confined as they are, sees itself and no passkey.
    assert_eq!(seen(&h, &at_portal).await, (2, 1));
    assert_eq!(seen(&h, &elsewhere).await, (1, 0));

    let delete = pb::DeletePortalRequest {
        mutation: mutation("gone").into(),
        id: portal.id.clone(),
        ..Default::default()
    };
    h.portals(h.root()).delete_portal(delete).await.unwrap();
    let request = pb::ExchangeRequest::default();
    let config =
        h.config(None).with_default_header("cookie", format!("__Host-korpis_refresh={cookie}"));
    let ended = pb::SessionServiceClient::new(h.http.clone(), config).exchange(request).await;
    assert_eq!(ended.unwrap_err().code, ErrorCode::Unauthenticated, "its sessions ended");
    assert_eq!(h.effects("account.factor_removed").await.len(), 1, "its passkey removed");
    assert_eq!(seen(&h, &elsewhere).await, (1, 0), "the session begun elsewhere goes on");
}
