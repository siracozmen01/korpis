// SPDX-License-Identifier: AGPL-3.0-or-later

//! The CLI's sign-in (§9.4 of docs/08-identity.md, RFC 8628): a device authorization polled until
//! a recently signed-in account approves its user code, then spent by the poll that begins the
//! CLI's session; polled too soon it slows, denied or expired it is over.

use crate::support::{Fixture, browser, client, fixture, in_session, invite, redeem, signed_in};
use korpis_app::AppError;
use korpis_app::account::{Authorized, DeviceSignedIn};
use korpis_core::account::session::SessionKind;
use std::time::Duration;

fn conflict(e: &AppError, expected: &str) -> bool {
    matches!(e, AppError::Conflict { reason, .. } if *reason == expected)
}

async fn poll(f: &Fixture, a: &Authorized) -> Result<DeviceSignedIn, AppError> {
    f.app.device_token(a.device_code.expose(), &client()).await
}

#[tokio::test]
async fn an_approved_user_code_begins_one_cli_session_and_is_spent() {
    let f = fixture().await;
    let redeemed = redeem(&f, &invite(&f, "A Friend").await, "friend", &browser()).await.unwrap();
    let a = f.app.authorize_device(&client()).await.unwrap();
    assert_eq!(a.user_code.len(), 9, "two groups of four: {}", a.user_code);
    assert!(a.device_code.expose().starts_with("korpisdv_"));

    assert!(conflict(&poll(&f, &a).await.unwrap_err(), "device.pending"));
    let soon = poll(&f, &a).await.unwrap_err();
    let AppError::RateLimited { retry_after, .. } = soon else { panic!("{soon:?}") };
    assert_eq!(retry_after, Duration::from_secs(10), "five seconds slower");
    f.clock.advance(Duration::from_secs(11));

    let me = in_session(&redeemed);
    let shown = f.app.device_authorization(&me, &a.user_code.to_lowercase()).await.unwrap();
    assert_eq!(shown.source, "unknown", "the approver is shown where it asked from");
    f.app.decide_device(&me, &a.user_code, true).await.unwrap();
    let again = f.app.decide_device(&me, &a.user_code, true).await.unwrap_err();
    assert!(matches!(again, AppError::NotFound { .. }), "decided once");

    let signed_in = poll(&f, &a).await.unwrap();
    assert_eq!(signed_in.account, redeemed.account);
    assert!(signed_in.session.device.is_none(), "a CLI keeps no device cookie");
    let sessions = f.app.sessions(&me).await.unwrap();
    assert!(sessions.iter().any(|s| s.session.kind == SessionKind::Cli));
    f.clock.advance(Duration::from_secs(11));
    assert_eq!(poll(&f, &a).await.unwrap_err(), AppError::Unauthenticated, "spent");
}

#[tokio::test]
async fn a_denied_or_expired_authorization_is_over_and_approving_needs_a_recent_sign_in() {
    let f = fixture().await;
    let redeemed = redeem(&f, &invite(&f, "A Friend").await, "friend", &browser()).await.unwrap();
    let me = in_session(&redeemed);

    let denied = f.app.authorize_device(&client()).await.unwrap();
    let token = signed_in(redeemed.account);
    let stale = f.app.decide_device(&token, &denied.user_code, true).await.unwrap_err();
    assert!(conflict(&stale, "session.not_recent"), "{stale:?}");
    f.app.decide_device(&token, &denied.user_code, false).await.unwrap();
    assert!(conflict(&poll(&f, &denied).await.unwrap_err(), "device.denied"));

    let expired = f.app.authorize_device(&client()).await.unwrap();
    f.clock.advance(Duration::from_secs(11 * 60));
    let late = f.app.decide_device(&me, &expired.user_code, false).await.unwrap_err();
    assert!(matches!(late, AppError::NotFound { .. }), "{late:?}");
    assert!(conflict(&poll(&f, &expired).await.unwrap_err(), "device.expired"));
    assert_eq!(poll(&f, &expired).await.unwrap_err(), AppError::Unauthenticated, "forgotten");
}

#[tokio::test]
async fn an_approval_is_withdrawn_when_the_session_that_gave_it_ends() {
    let f = fixture().await;
    let redeemed = redeem(&f, &invite(&f, "A Friend").await, "friend", &browser()).await.unwrap();
    let me = in_session(&redeemed);
    let a = f.app.authorize_device(&client()).await.unwrap();
    f.app.decide_device(&me, &a.user_code, true).await.unwrap();
    f.app.sign_out(redeemed.session.refresh.expose()).await.unwrap();

    let withdrawn = poll(&f, &a).await.unwrap_err();
    assert!(conflict(&withdrawn, "device.denied"), "{withdrawn:?}");
    let sessions = f.app.sessions(&signed_in(redeemed.account)).await.unwrap();
    assert!(sessions.is_empty(), "no session was begun: {sessions:?}");
}
