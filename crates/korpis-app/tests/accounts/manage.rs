// SPDX-License-Identifier: AGPL-3.0-or-later

//! A signed-in user managing their own factors: nothing leaves the account without a way to sign
//! in, or a password without a lasting second factor beside it.

use crate::support::{browser, code, fixture, invite, redeem, signed_in};
use korpis_app::AppError;
use korpis_app::account::NewRegistration;
use korpis_core::account::totp::TotpSecret;

fn last_factor(e: &AppError) -> bool {
    matches!(e, AppError::Conflict { reason: "account.last_factor", .. })
}

#[tokio::test]
async fn the_last_way_to_sign_in_cannot_be_removed() {
    let f = fixture().await;
    let invitation = invite(&f, "A Friend").await;
    let phone = browser();
    let account = redeem(&f, &invitation, "friend", &phone).await.unwrap().account;
    let me = signed_in(account);

    let refused = f.app.remove_passkey(&me, phone.credential_id()).await.unwrap_err();
    assert!(last_factor(&refused), "{refused:?}");

    let laptop = browser();
    let options = f.app.begin_add_passkey(&me).await.unwrap();
    assert_eq!(options.exclude, vec![phone.credential_id().to_vec()]);
    let created = laptop.create(&options.challenge);
    let r = NewRegistration {
        challenge: options.challenge_id,
        label: "laptop".into(),
        client_data_json: created.client_data_json,
        attestation_object: created.attestation_object,
    };
    f.app.add_passkey(&me, r).await.unwrap();
    f.app.remove_passkey(&me, phone.credential_id()).await.unwrap();
    let view = f.app.own_account(&me).await.unwrap();
    assert_eq!(view.passkeys.len(), 1);
    assert_eq!(view.passkeys[0].label, "laptop");
}

#[tokio::test]
async fn a_password_keeps_a_passkey_or_totp_beside_it() {
    let f = fixture().await;
    let invitation = invite(&f, "A Friend").await;
    let phone = browser();
    let account = redeem(&f, &invitation, "friend", &phone).await.unwrap().account;
    let me = signed_in(account);

    f.app.set_password(&me, "correct horse battery").await.unwrap();
    let refused = f.app.remove_passkey(&me, phone.credential_id()).await.unwrap_err();
    assert!(last_factor(&refused), "a password alone would be left");

    let totp = TotpSecret::generate().unwrap();
    f.app.enrol_totp(&me, &totp, &code(&totp, 0)).await.unwrap();
    f.app.remove_passkey(&me, phone.credential_id()).await.unwrap();
    let refused = f.app.remove_totp(&me).await.unwrap_err();
    assert!(last_factor(&refused), "TOTP is the password's only lasting second factor now");

    let codes = f.app.renew_recovery_codes(&me).await.unwrap();
    assert_eq!(codes.len(), 10);
    let view = f.app.own_account(&me).await.unwrap();
    assert!(view.factors.password && view.factors.totp && view.factors.passkeys == 0);
}

#[tokio::test]
async fn factors_are_their_holder_s_alone() {
    let f = fixture().await;
    let refused = f.app.own_account(&f.root).await.unwrap_err();
    assert!(matches!(refused, AppError::Invalid { .. }), "a token has no factors");
}
