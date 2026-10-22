// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in through an identity provider, completed (§9.7 of docs/08-identity.md), against a
//! real control plane and a provider written with the SDK. A person returns with the state and a
//! code; the state is spent once, the verifier must be the challenge's, the provider asserts the
//! code beside the sign-in's own nonce, and the presentation is accepted once. The person's
//! session is theirs, their records as members follow what each sign-in named, and signing in
//! again from their own session re-authenticates it rather than beginning another.

use crate::bindings::{bind, set};
use crate::external_sign_in::{CHALLENGE, anonymous, begin, issuing, query};
use crate::failures::recorded;
use crate::panel::refresh_cookie;
use crate::support::Harness;
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_proto::korpis::v1 as pb;

/// The verifier [`CHALLENGE`] was made from.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

/// The session service, called with `bearer` if given and the refresh cookie `cookie` if given.
fn sessions(
    h: &Harness,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> pb::SessionServiceClient<HttpClient> {
    let mut config = h.config(bearer);
    if let Some(c) = cookie {
        config = config.with_default_header("cookie", format!("__Host-korpis_refresh={c}"));
    }
    pb::SessionServiceClient::new(h.http.clone(), config)
}

/// Begins a sign-in through `corp`; the state, and the code the identity provider would return
/// for `user` in `sets`, presented as `presentation`.
async fn returned(h: &Harness, user: &str, sets: &str, presentation: &str) -> (String, String) {
    let anonymous = anonymous(h);
    let begun = anonymous.begin_external_sign_in(begin(&h.root_org(), "corp", CHALLENGE)).await;
    let url = begun.unwrap().into_owned().url;
    let code = format!("{user}|{}|{sets}|{presentation}", query(&url, "nonce"));
    (query(&url, "state").to_owned(), code)
}

fn completion(state: &str, code: &str, verifier: &str) -> pb::CompleteExternalSignInRequest {
    pb::CompleteExternalSignInRequest {
        state: state.to_owned(),
        code: code.to_owned(),
        code_verifier: verifier.to_owned(),
        ..Default::default()
    }
}

/// Waits until the control plane's own loop has asked about `binding` the first time, after
/// which it is not due again for its interval: no re-verification then asks about a person
/// signed in meanwhile, whom this test's provider would say is in every set.
async fn first_asked(h: &Harness, binding: &str) {
    let identity = h.identity(h.root());
    let get = pb::GetIdentityBindingRequest { id: binding.to_owned(), ..Default::default() };
    for _ in 0..120 {
        let read = identity.get_identity_binding(get.clone()).await.unwrap().into_owned();
        let state = read.binding.as_option().and_then(|b| b.state.as_known());
        if state == Some(pb::BindingState::BINDING_STATE_VERIFIED) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("binding {binding} was never asked about");
}

/// Each member record kept, as `<set's member_of> <state>`, in order.
async fn records(h: &Harness) -> Vec<String> {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar(
        "SELECT split_part(b.subject, ':', 5) || ' ' || m.state
           FROM identity_member m JOIN identity_binding b ON b.id = m.binding_id
          ORDER BY 1",
    )
    .fetch_all(store.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_person_signs_in_once_per_state_into_a_session_of_their_own() {
    let h = Harness::new().await;
    let _corp = issuing(&h, 71, "corp", true).await;
    let org = h.root_org();
    for group in ["admins", "old"] {
        let made = bind(&h, &org, set(&org, group), pb::IdentityBindingTerms::default()).await;
        first_asked(&h, &made.unwrap().id).await;
    }

    let (state, code) = returned(&h, "dana", "admins,old,unbound", "p1").await;
    let done = anonymous(&h).complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    let done = done.unwrap();
    let cookie = refresh_cookie(done.headers()).expect("the refresh credential is the cookie");
    let done = done.into_owned();
    assert!(done.binding.starts_with("idb_"), "{done:?}");
    assert!(done.session.starts_with("ses_"));
    assert!(done.reauthenticated_at.as_option().is_none());
    assert_eq!(records(&h).await, ["admins verified", "old verified"], "bound sets only");

    let again = anonymous(&h).complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    assert_eq!(again.unwrap_err().code, ErrorCode::Unauthenticated, "a state is spent once");

    let browser = sessions(&h, Some(&cookie), None);
    let exchanged = browser.exchange(pb::ExchangeRequest::default()).await.unwrap().into_owned();
    assert!(exchanged.access_token.starts_with("korpisax_"), "a person's access token");
    assert_eq!((exchanged.account.as_str(), &exchanged.binding), ("", &done.binding));
    let token = exchanged.access_token;
    let signed_in = sessions(&h, None, Some(&token));
    let listed = signed_in.list_sessions(pb::ListSessionsRequest::default()).await;
    let listed = listed.unwrap().into_owned().sessions;
    assert_eq!(listed.len(), 1);
    assert!(listed[0].current);
    let profiles = pb::ProfileServiceClient::new(h.http.clone(), h.config(Some(&token)));
    let profile = profiles.get_profile(pb::GetProfileRequest::default()).await.unwrap();
    let profile = profile.into_owned();
    let person = profile.person.as_option().expect("a person's profile is their binding");
    assert_eq!((person.id.as_str(), person.provider.as_str()), (done.binding.as_str(), "corp"));
    assert_eq!((profile.account.as_str(), profile.passkeys.len()), ("", 0), "and no account");

    // Signed in again from their own session, the same person re-authenticates it, and the
    // sets this sign-in no longer names lose their record.
    let (state, code) = returned(&h, "dana", "admins", "p2").await;
    let again = sessions(&h, None, Some(&token))
        .complete_external_sign_in(completion(&state, &code, VERIFIER))
        .await
        .unwrap()
        .into_owned();
    assert_eq!(again.session, done.session, "no other session begins");
    assert!(again.reauthenticated_at.as_option().is_some());
    assert_eq!(records(&h).await, ["admins verified", "old unverified"]);

    // Another person's sign-in vouches for nobody's session but theirs.
    let (state, code) = returned(&h, "erin", "admins", "p3").await;
    let other = sessions(&h, None, Some(&token))
        .complete_external_sign_in(completion(&state, &code, VERIFIER))
        .await;
    assert_eq!(other.unwrap_err().code, ErrorCode::Unauthenticated);
}

#[tokio::test]
async fn a_wrong_verifier_spends_the_state_and_a_presentation_is_accepted_once() {
    let h = Harness::new().await;
    let _corp = issuing(&h, 72, "corp", true).await;
    let sessions = anonymous(&h);
    let refused = |r: Result<_, connectrpc::ConnectError>| r.map(|_| ()).unwrap_err().code;

    let (state, code) = returned(&h, "dana", "", "p1").await;
    let other = "a".repeat(43);
    let wrong = sessions.complete_external_sign_in(completion(&state, &code, &other)).await;
    assert_eq!(refused(wrong), ErrorCode::Unauthenticated, "not the challenge's verifier");
    let late = sessions.complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    assert_eq!(refused(late), ErrorCode::Unauthenticated, "the state was spent by the attempt");

    let (state, code) = returned(&h, "eve", "", "p2").await;
    let nobody = sessions.complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    assert_eq!(refused(nobody), ErrorCode::Unauthenticated, "a code its provider refuses");

    let (state, code) = returned(&h, "dana", "", "p3").await;
    let first = sessions.complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    assert!(first.is_ok(), "{first:?}");
    let (state, code) = returned(&h, "dana", "", "p3").await;
    let replay = sessions.complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    assert_eq!(refused(replay), ErrorCode::Unauthenticated, "the presentation was accepted");

    let counted =
        |f: &[(String, i64, Option<uuid::Uuid>, String)]| f.iter().map(|r| r.1).sum::<i64>();
    let failed = recorded(&h, "session.external", |f| counted(f) >= 4).await;
    assert_eq!(counted(&failed), 4, "each refusal is a failed attempt: {failed:?}");
    let store = h.db.store().await.unwrap();
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_call
          WHERE request ? 'assert' AND (request #> '{assert,headers}') ? 'code_verifier'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(kept, 0, "no asserted call keeps its verifier");
}

/// Signs `user` in through `corp` in `sets`; their binding and an access token for the session.
pub async fn signed_in(
    h: &Harness,
    user: &str,
    sets: &str,
    presentation: &str,
) -> (String, String) {
    let (state, code) = returned(h, user, sets, presentation).await;
    let anonymous = anonymous(h);
    let done = anonymous.complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    let done = done.unwrap();
    let cookie = refresh_cookie(done.headers()).unwrap();
    let browser = sessions(h, Some(&cookie), None);
    let exchanged = browser.exchange(pb::ExchangeRequest::default()).await.unwrap().into_owned();
    (done.into_owned().binding, exchanged.access_token)
}

#[tokio::test]
async fn a_person_approves_korpis_login_and_the_cli_s_session_is_theirs() {
    let h = Harness::new().await;
    let _corp = issuing(&h, 73, "corp", true).await;
    let (binding, token) = signed_in(&h, "dana", "ops", "p1").await;
    let cli = anonymous(&h);
    let asked = cli.authorize_device(pb::AuthorizeDeviceRequest::default()).await.unwrap();
    let asked = asked.into_owned();
    let approve =
        || pb::ApproveDeviceRequest { user_code: asked.user_code.clone(), ..Default::default() };

    // A sign-in more than ten minutes old approves nothing: the person signs in again first.
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE user_session SET authenticated_at = now() - interval '11 minutes'")
        .execute(store.pool())
        .await
        .unwrap();
    let browser = sessions(&h, None, Some(&token));
    let old = browser.approve_device(approve()).await.unwrap_err();
    assert_eq!(old.code, ErrorCode::Aborted, "session.not_recent: {old:?}");
    let (state, code) = returned(&h, "dana", "ops", "p2").await;
    let again = browser.complete_external_sign_in(completion(&state, &code, VERIFIER)).await;
    assert!(again.unwrap().into_owned().reauthenticated_at.as_option().is_some());
    browser.approve_device(approve()).await.unwrap();

    let poll =
        pb::DeviceTokenRequest { device_code: asked.device_code.clone(), ..Default::default() };
    let signed_in = cli.device_token(poll).await.unwrap().into_owned();
    assert_eq!((signed_in.account.as_str(), &signed_in.binding), ("", &binding), "hers");
    let refresh = pb::ExchangeRequest { refresh: signed_in.refresh, ..Default::default() };
    let exchanged = cli.exchange(refresh).await.unwrap().into_owned();
    assert!(exchanged.access_token.starts_with("korpisax_"));
    let held: (String,) = sqlx::query_as(
        "SELECT sets::text FROM user_session WHERE kind = 'cli' AND binding_id IS NOT NULL",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert!(held.0.contains("\"ops\""), "the CLI holds the sets her browser does: {}", held.0);
}
