// SPDX-License-Identifier: AGPL-3.0-or-later

//! An invitation redeemed by registering a passkey, and signing in with it.

use crate::support::{browser, fixture, invite, invite_until, redeem};
use korpis_app::AppError;
use korpis_app::account::{Assertion, NewRegistration};
use korpis_app::ports::{AccountRead as _, EffectFilter, EffectRead as _};
use korpis_core::account::record::INVITATION_LIFETIME;
use korpis_core::account::recovery::CODES;
use std::time::{Duration, SystemTime};

#[tokio::test]
async fn an_invitation_registers_one_passkey_once() {
    let f = fixture().await;
    let invitation = invite(&f, "A Friend").await;
    let view = f.app.invitation(&invitation).await.unwrap();
    assert_eq!(view.display_name, "A Friend");
    assert!(!view.passkey_only);

    let phone = browser();
    let redeemed = redeem(&f, &invitation, "Friend", &phone).await.unwrap();
    assert_eq!(redeemed.recovery_codes.len(), CODES);
    let account = f.app.store().account(redeemed.account).await.unwrap().unwrap();
    assert_eq!(account.name.as_deref(), Some("friend"), "lowercased");
    assert_eq!(f.app.store().factors(account.id).await.unwrap().passkeys, 1);

    assert_eq!(f.app.invitation(&invitation).await, Err(AppError::Unauthenticated), "spent");
    let again = redeem(&f, &invitation, "friend2", &browser()).await;
    assert_eq!(again.unwrap_err(), AppError::Unauthenticated);
    assert_eq!(f.app.invitation("korpisiv_nothing").await, Err(AppError::Unauthenticated));
}

#[tokio::test]
async fn a_sign_in_name_is_one_account_s_and_an_invitation_lasts_a_week_at_most() {
    let f = fixture().await;
    let (one, two) = (invite(&f, "One").await, invite(&f, "Two").await);
    redeem(&f, &one, "same", &browser()).await.unwrap();
    let taken = f.app.begin_redemption(&two, "SAME").await.unwrap_err();
    assert!(matches!(taken, AppError::Conflict { reason: "account.name_taken", .. }), "{taken:?}");
    let bad = f.app.begin_redemption(&two, "-x").await.unwrap_err();
    assert!(matches!(bad, AppError::Invalid { .. }));

    let too_late = SystemTime::now() + INVITATION_LIFETIME + Duration::from_secs(60);
    let refused = invite_until(&f, "Three", Some(too_late)).await.unwrap_err();
    assert!(matches!(refused, AppError::Invalid { .. }), "{refused:?}");
}

#[tokio::test]
async fn a_passkey_signs_in_once_per_challenge_and_a_cloned_one_is_recorded() {
    let f = fixture().await;
    let invitation = invite(&f, "A Friend").await;
    let mut phone = browser();
    let account = redeem(&f, &invitation, "friend", &phone).await.unwrap().account;

    let options = f.app.begin_sign_in().await.unwrap();
    let got = phone.get(&options.challenge);
    let assertion = Assertion {
        challenge: options.challenge_id,
        credential_id: got.credential_id,
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
    };
    let signed_in =
        f.app.sign_in_with_passkey(assertion.clone(), &crate::support::client()).await.unwrap();
    assert_eq!(signed_in.account, account);
    let replayed = f.app.sign_in_with_passkey(assertion, &crate::support::client()).await;
    assert_eq!(replayed, Err(AppError::Unauthenticated), "the challenge was taken");

    // Another authenticator holding the credential answers with a counter behind the stored one.
    phone.set_counter(0);
    let options = f.app.begin_sign_in().await.unwrap();
    let got = phone.get(&options.challenge);
    let cloned = Assertion {
        challenge: options.challenge_id,
        credential_id: got.credential_id,
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
    };
    assert_eq!(
        f.app.sign_in_with_passkey(cloned, &crate::support::client()).await,
        Err(AppError::Unauthenticated)
    );
    let all = EffectFilter { workload: None, plan: None, node: None, since: None };
    let effects = f.app.store().effects(&all, None, 1000).await.unwrap();
    let cloned = effects.iter().find(|e| e.action == "account.passkey_cloned").unwrap();
    assert_eq!(cloned.actor, korpis_core::principal::Principal::User(account));
}

#[tokio::test]
async fn a_registration_answering_another_invitation_s_challenge_is_refused() {
    let f = fixture().await;
    let (one, two) = (invite(&f, "One").await, invite(&f, "Two").await);
    let options = f.app.begin_redemption(&one, "one").await.unwrap();
    let created = browser().create(&options.challenge);
    let r = NewRegistration {
        challenge: options.challenge_id,
        label: String::new(),
        client_data_json: created.client_data_json,
        attestation_object: created.attestation_object,
    };
    let crossed = f.app.redeem_with_passkey(&two, ("two", r), &crate::support::client()).await;
    assert_eq!(crossed.unwrap_err(), AppError::Unauthenticated);
    assert!(f.app.invitation(&two).await.is_ok(), "the other invitation is still open");
}
