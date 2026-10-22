// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the sign-in page at a portal sees (§9.8 of docs/08-identity.md, §7 of
//! docs/11-surface-web.md): `GetServerInfo` names the portal and its organization,
//! `GetSignInTheme` answers the theme governing that organization, and `ListSignInProviders` its
//! providers; a sign-in through one comes back to the portal's own callback and is completed
//! there alone, into a session begun at the portal; and a provider not at or beneath the portal's
//! organization is not reached there.

use crate::domains::child;
use crate::external_sign_in::{CHALLENGE, anonymous, begin, issuing, query};
use crate::panel::refresh_cookie;
use crate::portal_client::{Portal, portal_of};
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

/// RFC 7636's own verifier, of [`CHALLENGE`].
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

/// Pinecone's theme, set on `org`.
async fn themed(h: &Harness, org: &str) {
    let theme = pb::Theme { product_name: "Pinecone".into(), ..Default::default() };
    let set = pb::SetThemeRequest {
        mutation: mutation("theme").into(),
        organization: org.into(),
        theme: Some(theme).into(),
        ..Default::default()
    };
    h.orgs(h.root()).set_theme(set).await.unwrap();
}

/// The theme the sign-in page at `portal` wears.
async fn worn(portal: &Portal) -> pb::GoverningTheme {
    let asked = pb::GetSignInThemeRequest::default();
    let answer = portal.call("korpis.v1.ServerInfoService/GetSignInTheme", &asked, None).await;
    let (got, _): (pb::GetSignInThemeResponse, _) = answer.unwrap();
    got.governing.into_option().unwrap()
}

/// The providers the sign-in page at `portal` offers, by name.
async fn offered(portal: &Portal) -> Vec<String> {
    let asked = pb::ListSignInProvidersRequest::default();
    let answer = portal.call("korpis.v1.SessionService/ListSignInProviders", &asked, None).await;
    let (got, _): (pb::ListSignInProvidersResponse, _) = answer.unwrap();
    got.providers.into_iter().map(|p| p.name).collect()
}

/// A sign-in through the root organization's `corp` begun at `portal`: the address the person is
/// sent to.
async fn begun_at(portal: &Portal, root: &str) -> Result<String, reqwest::StatusCode> {
    let asked = begin(root, "corp", CHALLENGE);
    let answer = portal.call("korpis.v1.SessionService/BeginExternalSignIn", &asked, None).await;
    let (got, _): (pb::BeginExternalSignInResponse, _) = answer?;
    Ok(got.url)
}

/// What the person returns with from `url`, signed in as dana.
fn returned(url: &str) -> pb::CompleteExternalSignInRequest {
    pb::CompleteExternalSignInRequest {
        state: query(url, "state").into(),
        code: format!("dana|{}||p-{}", query(url, "nonce"), &query(url, "state")[..8]),
        code_verifier: VERIFIER.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_portal_s_sign_in_page_wears_its_organization_s_theme_and_offers_its_providers() {
    let h = Harness::new().await;
    let _corp = issuing(&h, 81, "corp", true).await;
    let root = h.root_org();
    let reseller = child(&h, "pinecone").await;
    themed(&h, &reseller).await;
    let home = portal_of(&h, ("home", &root), ("kestrel.example", "panel.kestrel.example")).await;
    let shop = portal_of(&h, ("shop", &reseller), ("pinecone.example", "panel.pinecone.example"));
    let shop = shop.await;

    let asked = pb::GetServerInfoRequest::default();
    let info = shop.call("korpis.v1.ServerInfoService/GetServerInfo", &asked, None).await;
    let (info, _): (pb::GetServerInfoResponse, _) = info.unwrap();
    assert_eq!((info.portal, info.portal_organization), (shop.id.clone(), reseller.clone()));
    let own = pb::ServerInfoServiceClient::new(h.http.clone(), h.config(None));
    let own = own.get_server_info(asked).await.unwrap().into_owned();
    assert_eq!((own.portal.as_str(), own.portal_organization.as_str()), ("", ""));

    let theme = worn(&shop).await;
    assert_eq!(theme.set_on.as_deref(), Some(reseller.as_str()));
    assert_eq!(theme.theme.as_option().unwrap().product_name, "Pinecone");
    assert_eq!(worn(&home).await.set_on, None, "the root organization sets none");

    assert_eq!(offered(&home).await, ["corp"]);
    assert!(offered(&shop).await.is_empty(), "the root's provider is not Pinecone's");
    assert_eq!(begun_at(&shop, &root).await, Err(reqwest::StatusCode::NOT_FOUND));

    // A sign-in begun at a portal comes back there, and is completed nowhere else.
    let url = begun_at(&home, &root).await.unwrap();
    let callback = format!("{}/sign-in/callback", home.origin);
    assert_eq!(query(&url, "redirect_uri"), callback);
    let elsewhere = anonymous(&h).complete_external_sign_in(returned(&url)).await;
    assert_eq!(elsewhere.unwrap_err().code, ErrorCode::Unauthenticated);
    let url = begun_at(&home, &root).await.unwrap();
    let back = returned(&url);
    let done = home.call("korpis.v1.SessionService/CompleteExternalSignIn", &back, None);
    let (_, headers): (pb::CompleteExternalSignInResponse, _) = done.await.unwrap();
    assert!(refresh_cookie(&headers).is_some());
    let store = h.db.store().await.unwrap();
    let at_portal: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_session s JOIN portal p ON p.id = s.portal_id
          WHERE p.name = 'panel.kestrel.example' AND s.binding_id IS NOT NULL",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(at_portal, 1, "the person's session began at the portal");
}
