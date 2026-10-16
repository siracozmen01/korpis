// SPDX-License-Identifier: AGPL-3.0-or-later

//! Device cookies (§9.6 of docs/08-identity.md): a stranger spending a name's guesses keeps its
//! holder out of new browsers only, a browser that has signed in is charged to its own guesses
//! and, once it has spent them, as a browser without a cookie, and a cookie for another account
//! or one this control plane did not sign is not honoured.

use crate::support::{Fixture, client, code, fixture, invite};
use korpis_app::AppError;
use korpis_app::account::{Client, PasswordRedemption, SecondFactor};
use zeroize::Zeroizing;

const PASSWORD: &str = "correct horse battery";

/// Redeems an invitation as `name` with a password, answering the recovery codes and the
/// browser that redeemed it, holding the device cookie the redemption gave it.
async fn redeem(f: &Fixture, name: &str) -> (Vec<String>, Client) {
    let invitation = invite(f, "A Friend").await;
    let totp = korpis_core::account::totp::TotpSecret::generate().unwrap();
    let r = PasswordRedemption {
        name: name.into(),
        password: Zeroizing::new(PASSWORD.into()),
        code: code(&totp, 0),
        totp,
    };
    let redeemed = f.app.redeem_with_password(&invitation, r, &client()).await.unwrap();
    let device = redeemed.session.device.expect("a browser is given a device cookie");
    assert_eq!(device.account, redeemed.account);
    let held = Client { devices: vec![device.cookie], ..client() };
    (redeemed.recovery_codes, held)
}

async fn guess(f: &Fixture, codes: &[String], password: &str, from: &Client) -> AppError {
    let second = SecondFactor::Recovery(codes[0].clone());
    f.app.sign_in_with_password("friend", password, second, from).await.unwrap_err()
}

#[tokio::test]
async fn a_name_s_spent_guesses_keep_out_only_browsers_without_its_cookie() {
    let f = fixture().await;
    let (codes, held) = redeem(&f, "friend").await;
    for _ in 0..10 {
        assert_eq!(
            guess(&f, &codes, "not the password!", &client()).await,
            AppError::Unauthenticated
        );
    }
    let stranger = guess(&f, &codes, PASSWORD, &client()).await;
    assert!(matches!(stranger, AppError::RateLimited { .. }), "{stranger:?}");
    let second = SecondFactor::Recovery(codes[0].clone());
    let holder = f.app.sign_in_with_password("friend", PASSWORD, second, &held).await;
    assert!(holder.is_ok(), "the holder's browser is spared: {holder:?}");
}

#[tokio::test]
async fn a_device_that_spent_its_own_guesses_is_charged_as_one_without_a_cookie() {
    let f = fixture().await;
    let (codes, held) = redeem(&f, "friend").await;
    for _ in 0..5 {
        assert_eq!(guess(&f, &codes, "not the password!", &held).await, AppError::Unauthenticated);
    }
    for _ in 0..10 {
        let spent = guess(&f, &codes, "not the password!", &held).await;
        assert_eq!(spent, AppError::Unauthenticated, "charged to the name now");
    }
    let refused = guess(&f, &codes, PASSWORD, &held).await;
    assert!(matches!(refused, AppError::RateLimited { .. }), "{refused:?}");
}

#[tokio::test]
async fn a_cookie_for_another_account_or_unsigned_is_not_honoured() {
    let f = fixture().await;
    let (codes, _) = redeem(&f, "friend").await;
    let (_, other) = redeem(&f, "other").await;
    let forged = Client { devices: vec!["korpisdc_forged.x".into()], ..client() };
    for from in [&other, &forged] {
        for _ in 0..5 {
            assert_eq!(
                guess(&f, &codes, "not the password!", from).await,
                AppError::Unauthenticated
            );
        }
    }
    let spent = guess(&f, &codes, PASSWORD, &other).await;
    assert!(matches!(spent, AppError::RateLimited { .. }), "ten charged to the name: {spent:?}");
}
