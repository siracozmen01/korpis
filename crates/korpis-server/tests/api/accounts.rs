// SPDX-License-Identifier: AGPL-3.0-or-later

//! The account service (§9.6 of docs/08-identity.md, §10 of docs/10-api.md): a grant issued to a
//! new user answers its invitation once; the invitation is redeemed with a passkey or a password
//! and TOTP, and the account signs in with either; every refused presentation is a failed attempt
//! charged to its source, and every call to the service is charged to its source besides.

use crate::panel::{accounts, browser, invite, redeem};
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_core::account::totp::{STEP_SECONDS, TotpSecret};
use korpis_core::rate::{Rate, Rates};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::sign_in_with_password_request::Second;
use std::time::{SystemTime, UNIX_EPOCH};

fn rates(individual: Rate, failures: Rate) -> Rates {
    let ample = Rate::per_second(1000, 1000);
    Rates {
        individual,
        organization: ample,
        expensive_individual: ample,
        expensive_organization: ample,
        failures,
        recorded: ample,
        guesses: ample,
    }
}

fn code(secret: &[u8], ahead: u64) -> String {
    let secret = TotpSecret::from_bytes(secret.try_into().unwrap());
    let step = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() / STEP_SECONDS;
    format!("{:06}", secret.code(step + ahead))
}

#[tokio::test]
async fn an_invitation_is_answered_once_and_redeemed_once_with_a_passkey_that_signs_in() {
    let h = Harness::new().await;
    let issued = invite(&h, "invite-friend", "A Friend").await;
    assert!(issued.invitation.starts_with("korpisiv_"));
    let replayed = invite(&h, "invite-friend", "A Friend").await;
    assert_eq!(replayed.invitation, "", "a replay does not carry the secret");
    assert_eq!(replayed.grant.as_option().unwrap().id, issued.grant.as_option().unwrap().id);

    let seen = accounts(&h)
        .get_invitation(pb::GetInvitationRequest {
            invitation: issued.invitation.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(seen.display_name, "A Friend");

    let mut phone = browser();
    let redeemed = redeem(&h, &issued.invitation, "friend", &phone).await.unwrap();
    assert!(redeemed.account.starts_with("usr_"));
    assert_eq!(redeemed.recovery_codes.len(), 10);
    let again = redeem(&h, &issued.invitation, "friend", &browser()).await.unwrap_err();
    assert_eq!(again.code, ErrorCode::Unauthenticated);

    let begun = accounts(&h).begin_sign_in(pb::BeginSignInRequest::default()).await.unwrap();
    let options = begun.into_owned().request.as_option().unwrap().clone();
    let got = phone.get(&options.challenge);
    let signed_in = accounts(&h)
        .sign_in_with_passkey(pb::SignInWithPasskeyRequest {
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
    assert_eq!(signed_in.account, redeemed.account);
    assert_eq!(h.effects("account.invited").await.len(), 1);
    assert_eq!(h.effects("account.redeemed").await.len(), 1);
    assert_eq!(h.effects("account.signed_in").await.len(), 1);
}

#[tokio::test]
async fn an_invitation_is_redeemed_with_a_password_and_totp_that_sign_in_together() {
    let h = Harness::new().await;
    let invitation = invite(&h, "invite-friend", "A Friend").await.invitation;
    let offer = accounts(&h)
        .offer_totp(pb::OfferTotpRequest { name: "friend".to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert!(offer.uri.starts_with("otpauth://totp/localhost:friend?"), "{}", offer.uri);
    let redeemed = accounts(&h)
        .redeem_with_password(pb::RedeemWithPasswordRequest {
            invitation,
            name: "friend".to_owned(),
            password: "correct horse battery".to_owned(),
            totp_secret: offer.secret.clone(),
            totp_code: code(&offer.secret, 0),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let sign_in = |second| pb::SignInWithPasswordRequest {
        name: "friend".to_owned(),
        password: "correct horse battery".to_owned(),
        second: Some(second),
        ..Default::default()
    };
    let next = Second::TotpCode(code(&offer.secret, 1));
    let signed_in =
        accounts(&h).sign_in_with_password(sign_in(next.clone())).await.unwrap().into_owned();
    assert_eq!(signed_in.account, redeemed.account);
    let spent = accounts(&h).sign_in_with_password(sign_in(next)).await.unwrap_err();
    assert_eq!(spent.code, ErrorCode::Unauthenticated);
}

/// Redeems a new invitation as `friend` with a password and TOTP; the TOTP secret.
async fn password_account(h: &Harness) -> Vec<u8> {
    let invitation = invite(h, "invite-friend", "A Friend").await.invitation;
    let offer = accounts(h)
        .offer_totp(pb::OfferTotpRequest { name: "friend".to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    accounts(h)
        .redeem_with_password(pb::RedeemWithPasswordRequest {
            invitation,
            name: "friend".to_owned(),
            password: "correct horse battery".to_owned(),
            totp_secret: offer.secret.clone(),
            totp_code: code(&offer.secret, 0),
            ..Default::default()
        })
        .await
        .unwrap();
    offer.secret
}

#[tokio::test]
async fn a_source_that_keeps_guessing_is_told_to_stop_and_then_nothing_is_checked() {
    let ample = Rate::per_second(1000, 1000);
    let h = Harness::with_rates(rates(ample, Rate::per_minute(2, 2))).await;
    let secret = password_account(&h).await;
    let attempt = |password: &str| pb::SignInWithPasswordRequest {
        name: "friend".to_owned(),
        password: password.to_owned(),
        second: Some(Second::TotpCode(code(&secret, 1))),
        ..Default::default()
    };
    for _ in 0..2 {
        let refused = accounts(&h).sign_in_with_password(attempt("a wrong guess")).await;
        assert_eq!(refused.unwrap_err().code, ErrorCode::Unauthenticated);
    }
    let right = accounts(&h).sign_in_with_password(attempt("correct horse battery")).await;
    let stopped = right.unwrap_err();
    assert_eq!(stopped.code, ErrorCode::ResourceExhausted, "the right password is not checked");
    assert!(stopped.response_headers().contains_key("retry-after"));
    assert!(!h.effects("account.sign_in").await.is_empty(), "failures are recorded");
}

#[tokio::test]
async fn guesses_at_many_names_sent_at_once_pass_no_more_than_the_source_s_failures() {
    let ample = Rate::per_second(1000, 1000);
    let h = Harness::with_rates(rates(ample, Rate::per_minute(2, 2))).await;
    let client = accounts(&h);
    let guess = |n: usize| {
        client.sign_in_with_password(pb::SignInWithPasswordRequest {
            name: format!("name-{n}"),
            password: "a wrong guess".to_owned(),
            second: Some(Second::RecoveryCode("AAAA-AAAA-AAAA-AAAA".to_owned())),
            ..Default::default()
        })
    };
    let answers = futures_util::future::join_all((0..20).map(guess)).await;
    let checked = answers
        .iter()
        .filter(|a| a.as_ref().is_err_and(|e| e.code == ErrorCode::Unauthenticated))
        .count();
    assert_eq!(checked, 2, "the source has room for two failures, however many are in flight");
}

#[tokio::test]
async fn every_call_to_the_service_is_charged_to_its_source() {
    let ample = Rate::per_second(1000, 1000);
    let h = Harness::with_rates(rates(Rate::per_minute(3, 3), ample)).await;
    for _ in 0..3 {
        accounts(&h).begin_sign_in(pb::BeginSignInRequest::default()).await.unwrap();
    }
    let stopped = accounts(&h).begin_sign_in(pb::BeginSignInRequest::default()).await;
    assert_eq!(stopped.unwrap_err().code, ErrorCode::ResourceExhausted);
}

#[tokio::test]
async fn a_new_user_is_the_only_holder_named() {
    let h = Harness::new().await;
    let refused = h
        .grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: crate::support::mutation("both").into(),
            actions: vec!["workload.read".to_owned()],
            scope: crate::support::org_scope(&h.root_org()).into(),
            to_new_user: pb::NewUser { display_name: "A Friend".to_owned(), ..Default::default() }
                .into(),
            to_new_token: true,
            label: "both".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
}
