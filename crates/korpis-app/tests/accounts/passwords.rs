// SPDX-License-Identifier: AGPL-3.0-or-later

//! An invitation redeemed with a password and TOTP, and signing in with the password and a code
//! or a recovery code beside it.

use crate::support::{Fixture, code, fixture, invite};
use korpis_app::AppError;
use korpis_app::account::{PasswordRedemption, Redeemed, SecondFactor};
use korpis_core::account::totp::TotpSecret;
use zeroize::Zeroizing;

const PASSWORD: &str = "correct horse battery";

async fn redeem(f: &Fixture, name: &str) -> (Redeemed, TotpSecret) {
    let invitation = invite(f, "A Friend").await;
    let totp = TotpSecret::generate().unwrap();
    let r = PasswordRedemption {
        name: name.into(),
        password: Zeroizing::new(PASSWORD.into()),
        totp: totp.clone(),
        code: code(&totp, 0),
    };
    (f.app.redeem_with_password(&invitation, r).await.unwrap(), totp)
}

#[tokio::test]
async fn a_password_signs_in_only_with_a_code_beside_it_and_each_code_once() {
    let f = fixture().await;
    let (redeemed, totp) = redeem(&f, "friend").await;
    let next = SecondFactor::Totp(code(&totp, 1));
    let signed_in =
        f.app.sign_in_with_password("friend", PASSWORD, next.clone(), None).await.unwrap();
    assert_eq!(signed_in.account, redeemed.account);
    assert!(signed_in.mfa);
    let again = f.app.sign_in_with_password("friend", PASSWORD, next, None).await;
    assert_eq!(again, Err(AppError::Unauthenticated), "the step is spent");

    let wrong = SecondFactor::Totp("000000".into());
    let refused = f.app.sign_in_with_password("friend", PASSWORD, wrong, None).await;
    assert_eq!(refused, Err(AppError::Unauthenticated));
}

#[tokio::test]
async fn a_recovery_code_stands_beside_a_password_once() {
    let f = fixture().await;
    let (redeemed, _) = redeem(&f, "friend").await;
    let recovery = SecondFactor::Recovery(redeemed.recovery_codes[3].to_lowercase());
    let signed_in = f.app.sign_in_with_password("friend", PASSWORD, recovery.clone(), None).await;
    assert_eq!(signed_in.unwrap().account, redeemed.account);
    let spent = f.app.sign_in_with_password("friend", PASSWORD, recovery, None).await;
    assert_eq!(spent, Err(AppError::Unauthenticated));
}

#[tokio::test]
async fn every_refusal_is_the_same_whichever_part_failed() {
    let f = fixture().await;
    let (redeemed, _) = redeem(&f, "friend").await;
    let recovery = || SecondFactor::Recovery(redeemed.recovery_codes[0].clone());
    for (name, password) in
        [("friend", "not the password at all"), ("nobody", PASSWORD), ("not a name", PASSWORD)]
    {
        let refused = f.app.sign_in_with_password(name, password, recovery(), None).await;
        assert_eq!(refused, Err(AppError::Unauthenticated), "{name}");
    }
    let unspent = f.app.sign_in_with_password("friend", PASSWORD, recovery(), None).await;
    assert!(unspent.is_ok(), "a refused password spends no recovery code");
}

#[tokio::test]
async fn a_redemption_refuses_a_short_password_and_a_code_of_another_secret() {
    let f = fixture().await;
    let invitation = invite(&f, "A Friend").await;
    let totp = TotpSecret::generate().unwrap();
    let other = TotpSecret::generate().unwrap();
    let attempt = |password: &str, code: String| PasswordRedemption {
        name: "friend".into(),
        password: Zeroizing::new(password.into()),
        totp: totp.clone(),
        code,
    };
    let short = f.app.redeem_with_password(&invitation, attempt("short", code(&totp, 0))).await;
    assert!(matches!(short, Err(AppError::Invalid { ref field, .. }) if field == "password"));
    let unread = f.app.redeem_with_password(&invitation, attempt(PASSWORD, code(&other, 0))).await;
    assert!(matches!(unread, Err(AppError::Invalid { ref field, .. }) if field == "code"));
    assert!(f.app.invitation(&invitation).await.is_ok(), "nothing was spent");
}

#[tokio::test]
async fn ten_wrong_guesses_at_a_name_refuse_even_the_right_password() {
    let f = fixture().await;
    let (redeemed, _) = redeem(&f, "friend").await;
    let recovery = || SecondFactor::Recovery(redeemed.recovery_codes[0].clone());
    for _ in 0..10 {
        let wrong = f.app.sign_in_with_password("Friend", "not the password!", recovery(), None);
        assert_eq!(wrong.await, Err(AppError::Unauthenticated));
    }
    let right = f.app.sign_in_with_password("friend", PASSWORD, recovery(), None).await;
    assert!(matches!(right, Err(AppError::RateLimited { .. })), "{right:?}");
    let other = f.app.sign_in_with_password("nobody", PASSWORD, recovery(), None).await;
    assert_eq!(other, Err(AppError::Unauthenticated), "another name is not slowed");
}
