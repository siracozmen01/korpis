// SPDX-License-Identifier: AGPL-3.0-or-later

//! Device cookies over the API (§9.6 of docs/08-identity.md): a redemption gives the browser a
//! host-bound cookie naming the account, and once a stranger has spent the sign-in name's
//! guesses, the browser holding it still signs in where one without it is refused.

use crate::panel::{accounts, invite};
use crate::support::Harness;
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_core::account::totp::{STEP_SECONDS, TotpSecret};
use korpis_core::rate::Rates;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::sign_in_with_password_request::Second;
use std::time::{SystemTime, UNIX_EPOCH};

const PASSWORD: &str = "correct horse battery";

fn code(secret: &[u8], ahead: u64) -> String {
    let secret = TotpSecret::from_bytes(secret.try_into().unwrap());
    let step = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() / STEP_SECONDS;
    format!("{:06}", secret.code(step + ahead))
}

/// The device cookie a response's `Set-Cookie` gives the browser, as a `Cookie` pair.
fn device_cookie(headers: &http::HeaderMap) -> Option<String> {
    let set = headers
        .get_all(http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("__Host-korpis_device_"))?;
    assert!(set.contains("; Path=/;") && set.contains("Secure") && !set.contains("Domain"));
    set.split(';').next().map(str::to_owned)
}

fn holding(h: &Harness, cookie: &str) -> pb::AccountServiceClient<HttpClient> {
    let config = h.config(None).with_default_header("cookie", cookie.to_owned());
    pb::AccountServiceClient::new(h.http.clone(), config)
}

fn attempt(password: &str, second: String) -> pb::SignInWithPasswordRequest {
    pb::SignInWithPasswordRequest {
        name: "friend".to_owned(),
        password: password.to_owned(),
        second: Some(Second::TotpCode(second)),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_spent_name_refuses_a_new_browser_and_not_the_one_that_signed_in() {
    let h = Harness::with_rates(Rates::default()).await;
    let invitation = invite(&h, "invite-friend", "A Friend").await.invitation;
    let offer = accounts(&h)
        .offer_totp(pb::OfferTotpRequest { name: "friend".to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    let redeemed = accounts(&h)
        .redeem_with_password(pb::RedeemWithPasswordRequest {
            invitation,
            name: "friend".to_owned(),
            password: PASSWORD.to_owned(),
            totp_secret: offer.secret.clone(),
            totp_code: code(&offer.secret, 0),
            ..Default::default()
        })
        .await
        .unwrap();
    let cookie = device_cookie(redeemed.headers()).expect("the redemption sets a device cookie");

    let new_browser = accounts(&h);
    for _ in 0..10 {
        let wrong = new_browser.sign_in_with_password(attempt("a wrong guess", "000000".into()));
        assert_eq!(wrong.await.unwrap_err().code, ErrorCode::Unauthenticated);
    }
    let stranger = new_browser.sign_in_with_password(attempt(PASSWORD, code(&offer.secret, 1)));
    assert_eq!(stranger.await.unwrap_err().code, ErrorCode::ResourceExhausted);
    let holder = holding(&h, &cookie)
        .sign_in_with_password(attempt(PASSWORD, code(&offer.secret, 1)))
        .await
        .unwrap();
    assert!(device_cookie(holder.headers()).is_some(), "each sign-in gives a new one");
    assert_eq!(holder.into_owned().account, redeemed.into_owned().account);
}
