// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions over the API (§9.4 of docs/08-identity.md): a redemption sets the refresh
//! credential as a cookie scoped to the session service, an exchange answers an access token the
//! rest of the API takes and replaces the cookie, a copy of a rotated cookie ends the session,
//! and the CLI's credential travels in the body instead.

use crate::panel::{browser, invite, redeem_keeping_cookie, refresh_cookie};
use crate::support::Harness;
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_proto::korpis::v1 as pb;

fn sessions(
    h: &Harness,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> pb::SessionServiceClient<HttpClient> {
    let mut config = h.config(bearer);
    if let Some(c) = cookie {
        config = config.with_default_header("cookie", format!("__Secure-korpis_refresh={c}"));
    }
    pb::SessionServiceClient::new(h.http.clone(), config)
}

#[tokio::test]
async fn a_cookie_is_exchanged_for_an_access_token_and_a_copy_of_it_ends_the_session() {
    let h = Harness::new().await;
    let invitation = invite(&h, "invite-friend", "A Friend").await.invitation;
    let (redeemed, first) =
        redeem_keeping_cookie(&h, &invitation, "friend", &browser()).await.unwrap();
    assert!(first.starts_with("korpisrt_"), "the redemption set the cookie");
    assert!(redeemed.session.starts_with("ses_"));

    let exchanged =
        sessions(&h, Some(&first), None).exchange(pb::ExchangeRequest::default()).await.unwrap();
    let second = refresh_cookie(exchanged.headers()).expect("the cookie is replaced");
    let exchanged = exchanged.into_owned();
    assert_eq!(exchanged.refresh, "", "a browser's credential never travels in the body");
    assert_eq!(exchanged.account, redeemed.account);

    let listed = sessions(&h, None, Some(&exchanged.access_token))
        .list_sessions(pb::ListSessionsRequest::default())
        .await
        .unwrap()
        .into_owned();
    assert_eq!(listed.sessions.len(), 1);
    assert!(listed.sessions[0].current);

    let copy = sessions(&h, Some(&first), None).exchange(pb::ExchangeRequest::default()).await;
    assert_eq!(copy.unwrap_err().code, ErrorCode::Unauthenticated);
    let after = sessions(&h, Some(&second), None).exchange(pb::ExchangeRequest::default()).await;
    assert_eq!(after.unwrap_err().code, ErrorCode::Unauthenticated, "the session ended");
    assert_eq!(h.effects("session.reused").await.len(), 1);
}

#[tokio::test]
async fn a_credential_sent_in_the_body_is_answered_in_the_body_and_signs_out() {
    let h = Harness::new().await;
    let invitation = invite(&h, "invite-friend", "A Friend").await.invitation;
    let (_, first) = redeem_keeping_cookie(&h, &invitation, "friend", &browser()).await.unwrap();
    let request = pb::ExchangeRequest { refresh: first, ..Default::default() };
    let exchanged = sessions(&h, None, None).exchange(request).await.unwrap();
    assert!(refresh_cookie(exchanged.headers()).is_none(), "no cookie for the CLI");
    let next = exchanged.into_owned().refresh;
    assert!(next.starts_with("korpisrt_"));

    let out = pb::SignOutRequest { refresh: next.clone(), ..Default::default() };
    sessions(&h, None, None).sign_out(out).await.unwrap();
    let request = pb::ExchangeRequest { refresh: next, ..Default::default() };
    let gone = sessions(&h, None, None).exchange(request).await;
    assert_eq!(gone.unwrap_err().code, ErrorCode::Unauthenticated);
}
