// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions (§9.4 of docs/08-identity.md): a sign-in begins one, its refresh credential is
//! exchanged for access tokens the gateway accepts and rotated each time, a rotated credential
//! presented again ends it, and its user lists and ends sessions.

use crate::support::{browser, fixture, fixture_with, invite, redeem};
use korpis_app::AppError;
use korpis_core::account::session::SessionLimits;
use korpis_core::principal::Principal;
use std::time::Duration;

#[tokio::test]
async fn a_refresh_credential_is_exchanged_once_for_an_access_token_the_gateway_takes() {
    let f = fixture().await;
    let invitation = invite(&f, "A Friend").await;
    let redeemed = redeem(&f, &invitation, "friend", &browser()).await.unwrap();
    let first = redeemed.session.refresh.expose().to_owned();

    let exchanged = f.app.exchange(&first).await.unwrap();
    assert_eq!((exchanged.account, exchanged.session), (redeemed.account, redeemed.session.id));
    let caller = f.app.authenticate(&exchanged.access, None).await.unwrap();
    assert_eq!(caller.principal, Principal::User(redeemed.account));
    assert_eq!(caller.session, Some(redeemed.session.id));
    assert!(caller.mfa);

    let second = exchanged.refresh.expose().to_owned();
    assert_eq!(f.app.exchange(&first).await.unwrap_err(), AppError::Unauthenticated, "rotated");
    let ended = f.app.exchange(&second).await.unwrap_err();
    assert_eq!(ended, AppError::Unauthenticated, "a copy presented ends the whole session");
    assert!(f.app.authenticate("korpisat_forged.x", None).await.is_err());
}

#[tokio::test]
async fn a_user_lists_their_sessions_and_ends_the_others() {
    let f = fixture().await;
    let invitation = invite(&f, "A Friend").await;
    let phone = browser();
    let first = redeem(&f, &invitation, "friend", &phone).await.unwrap();
    let options = f.app.begin_sign_in().await.unwrap();
    let mut phone = phone;
    let got = phone.get(&options.challenge);
    let assertion = korpis_app::account::Assertion {
        challenge: options.challenge_id,
        credential_id: got.credential_id,
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
    };
    let second = f.app.sign_in_with_passkey(assertion, &crate::support::client()).await.unwrap();
    let exchanged = f.app.exchange(second.session.refresh.expose()).await.unwrap();
    let me = f.app.authenticate(&exchanged.access, None).await.unwrap();

    let listed = f.app.sessions(&me).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|s| s.current && s.session.id == second.session.id));
    assert_eq!(f.app.end_other_sessions(&me).await.unwrap(), 1);
    let gone = f.app.exchange(first.session.refresh.expose()).await;
    assert_eq!(gone.unwrap_err(), AppError::Unauthenticated, "ended at once");
    f.app.end_session(&me, second.session.id).await.unwrap();
    assert!(f.app.sessions(&me).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_idle_session_ends() {
    let limits = SessionLimits { idle: Duration::from_secs(1), ..SessionLimits::default() };
    let f = fixture_with(limits).await;
    let invitation = invite(&f, "A Friend").await;
    let redeemed = redeem(&f, &invitation, "friend", &browser()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let idle = f.app.exchange(redeemed.session.refresh.expose()).await;
    assert_eq!(idle.unwrap_err(), AppError::Unauthenticated);
}
