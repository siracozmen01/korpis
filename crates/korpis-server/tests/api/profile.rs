// SPDX-License-Identifier: AGPL-3.0-or-later

//! The profile service (§9.6 of docs/08-identity.md): a user signed in to a session reads and
//! changes their own factors, re-authenticating with a passkey, and an API token has none to
//! change.

use crate::panel::{access, accounts, browser, invite, redeem_keeping_cookie};
use crate::support::Harness;
use connectrpc::client::HttpClient;
use korpis_proto::korpis::v1 as pb;

fn profile(h: &Harness, bearer: &str) -> pb::ProfileServiceClient<HttpClient> {
    pb::ProfileServiceClient::new(h.http.clone(), h.config(Some(bearer)))
}

#[tokio::test]
async fn a_signed_in_user_re_authenticates_and_changes_their_factors() {
    let h = Harness::new().await;
    let invitation = invite(&h, "invite-friend", "A Friend").await.invitation;
    let mut phone = browser();
    let (redeemed, cookie) =
        redeem_keeping_cookie(&h, &invitation, "friend", &phone).await.unwrap();
    let me = profile(&h, &access(&h, &cookie).await);

    let seen = me.get_profile(pb::GetProfileRequest::default()).await.unwrap().into_owned();
    assert_eq!((seen.account.as_str(), seen.name.as_str()), (redeemed.account.as_str(), "friend"));
    assert_eq!((seen.passkeys.len(), seen.password, seen.recovery_codes), (1, false, 10));
    assert!(seen.authenticated_at.is_set());

    let begun = accounts(&h).begin_sign_in(pb::BeginSignInRequest::default()).await.unwrap();
    let options = begun.into_owned().request.as_option().unwrap().clone();
    let got = phone.get(&options.challenge);
    let again = me
        .reauthenticate_with_passkey(pb::SignInWithPasskeyRequest {
            challenge_id: options.challenge_id,
            credential_id: got.credential_id,
            client_data_json: got.client_data_json,
            authenticator_data: got.authenticator_data,
            signature: got.signature,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert!(again.authenticated_at.is_set());

    let password =
        pb::SetPasswordRequest { password: "correct horse battery".into(), ..Default::default() };
    me.set_password(password).await.unwrap();
    let codes = me.renew_recovery_codes(pb::RenewRecoveryCodesRequest::default()).await.unwrap();
    assert_eq!(codes.into_owned().recovery_codes.len(), 10);
    let seen = me.get_profile(pb::GetProfileRequest::default()).await.unwrap().into_owned();
    assert!(seen.password);
    assert_eq!(h.effects("session.reauthenticated").await.len(), 1);
}

#[tokio::test]
async fn an_api_token_has_no_factors_to_change() {
    let h = Harness::new().await;
    let refused = pb::ProfileServiceClient::new(h.http.clone(), h.root())
        .renew_recovery_codes(pb::RenewRecoveryCodesRequest::default())
        .await
        .unwrap_err();
    assert_eq!(refused.code, connectrpc::ErrorCode::InvalidArgument, "{refused:?}");
}
