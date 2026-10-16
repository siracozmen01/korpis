// SPDX-License-Identifier: AGPL-3.0-or-later

//! A recent sign-in (§9.6 of docs/08-identity.md): a factor is changed only from a session that
//! signed in or re-authenticated within ten minutes, a caller without a session never changes
//! one, and a session re-authenticates with a factor of its own account and no other's.

use crate::support::{browser, fixture, in_session, invite, redeem, signed_in};
use korpis_app::AppError;
use korpis_app::account::{Assertion, SecondFactor};
use korpis_core::webauthn::testing::SoftAuthenticator;
use std::time::Duration;

fn not_recent(e: &AppError) -> bool {
    matches!(e, AppError::Conflict { reason: "session.not_recent", .. })
}

async fn assertion(f: &crate::support::Fixture, browser: &mut SoftAuthenticator) -> Assertion {
    let options = f.app.begin_sign_in().await.unwrap();
    let got = browser.get(&options.challenge);
    Assertion {
        challenge: options.challenge_id,
        credential_id: got.credential_id,
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
    }
}

#[tokio::test]
async fn a_stale_session_changes_nothing_until_it_re_authenticates() {
    let f = fixture().await;
    let mut phone = browser();
    let invitation = invite(&f, "A Friend").await;
    let redeemed = redeem(&f, &invitation, "friend", &phone).await.unwrap();
    let me = in_session(&redeemed);
    f.app.renew_recovery_codes(&me).await.unwrap();

    f.clock.advance(Duration::from_secs(11 * 60));
    let stale = f.app.set_password(&me, "correct horse battery").await.unwrap_err();
    assert!(not_recent(&stale), "{stale:?}");
    assert!(f.app.own_account(&me).await.is_ok(), "reading needs no recent sign-in");

    let a = assertion(&f, &mut phone).await;
    let at = f.app.reauthenticate_with_passkey(&me, a).await.unwrap();
    let (_, authenticated_at) = f.app.profile(&me).await.unwrap();
    let kept = authenticated_at.unwrap();
    let apart = at.duration_since(kept).unwrap_or_else(|e| e.duration());
    assert!(apart < Duration::from_millis(1), "kept to the microsecond: {kept:?} {at:?}");
    f.app.set_password(&me, "correct horse battery").await.unwrap();

    let second = SecondFactor::Recovery("not-a-code".into());
    let wrong = f
        .app
        .reauthenticate_with_password(&me, "not the password!", second, &crate::support::client())
        .await;
    assert_eq!(wrong, Err(AppError::Unauthenticated));
}

#[tokio::test]
async fn a_caller_without_a_session_or_with_another_s_passkey_is_refused() {
    let f = fixture().await;
    let mut phone = browser();
    let mut stranger = browser();
    let mine = redeem(&f, &invite(&f, "A Friend").await, "friend", &phone).await.unwrap();
    let theirs = redeem(&f, &invite(&f, "Other").await, "other", &stranger).await.unwrap();

    let token = signed_in(mine.account);
    let refused = f.app.renew_recovery_codes(&token).await.unwrap_err();
    assert!(not_recent(&refused), "an API token cannot re-authenticate: {refused:?}");
    let a = assertion(&f, &mut phone).await;
    assert!(not_recent(&f.app.reauthenticate_with_passkey(&token, a).await.unwrap_err()));

    let a = assertion(&f, &mut stranger).await;
    let other = f.app.reauthenticate_with_passkey(&in_session(&mine), a).await;
    assert_eq!(other, Err(AppError::Unauthenticated));
    assert!(f.app.renew_recovery_codes(&in_session(&theirs)).await.is_ok());
}
