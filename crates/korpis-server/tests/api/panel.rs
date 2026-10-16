// SPDX-License-Identifier: AGPL-3.0-or-later

//! The panel the tests sign in from: its origin, the site its passkeys are bound to, and the
//! account service it calls without a credential.

use crate::support::{Harness, mutation, org_scope};
use connectrpc::client::HttpClient;
use korpis_core::webauthn::testing::SoftAuthenticator;
use korpis_proto::korpis::v1 as pb;

/// The origin the tests' panel is served from: passkeys are bound to `localhost`.
pub const PANEL: &str = "https://localhost:7443";

/// The relying party the tests' control plane is configured with.
pub fn relying_party() -> korpis_core::webauthn::RelyingParty {
    korpis_core::webauthn::RelyingParty { id: "localhost".into(), origins: vec![PANEL.into()] }
}

/// The account service, called as the panel calls it: with no credential.
pub fn accounts(h: &Harness) -> pb::AccountServiceClient<HttpClient> {
    pb::AccountServiceClient::new(h.http.clone(), h.config(None))
}

/// A browser on the panel's origin, with a passkey of its own.
pub fn browser() -> SoftAuthenticator {
    SoftAuthenticator::new("localhost", PANEL)
}

/// Issues `workload.read` on the root organization to a new user named `display_name`, under
/// the idempotency key `key`; the response.
pub async fn invite(h: &Harness, key: &str, display_name: &str) -> pb::IssueGrantResponse {
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation(key).into(),
            actions: vec!["workload.read".to_owned()],
            scope: org_scope(&h.root_org()).into(),
            to_new_user: pb::NewUser {
                display_name: display_name.to_owned(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned()
}

/// Redeems `invitation` as `name` by registering `browser`'s passkey; the response.
pub async fn redeem(
    h: &Harness,
    invitation: &str,
    name: &str,
    browser: &SoftAuthenticator,
) -> Result<pb::RedeemResponse, connectrpc::ConnectError> {
    Ok(redeem_keeping_cookie(h, invitation, name, browser).await?.0)
}

/// The refresh credential a response's `Set-Cookie` gives the browser, if it gives one.
pub fn refresh_cookie(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get_all(http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| v.strip_prefix("__Host-korpis_refresh="))
        .and_then(|v| v.split(';').next())
        .map(str::to_owned)
}

/// [`redeem`], with the refresh credential its response set as the browser's cookie.
pub async fn redeem_keeping_cookie(
    h: &Harness,
    invitation: &str,
    name: &str,
    browser: &SoftAuthenticator,
) -> Result<(pb::RedeemResponse, String), connectrpc::ConnectError> {
    let begun = accounts(h)
        .begin_redemption(pb::BeginRedemptionRequest {
            invitation: invitation.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        })
        .await?
        .into_owned();
    let creation = begun.creation.as_option().unwrap().clone();
    let created = browser.create(&creation.challenge);
    let registration = pb::PasskeyRegistration {
        challenge_id: creation.challenge_id,
        label: "phone".to_owned(),
        client_data_json: created.client_data_json,
        attestation_object: created.attestation_object,
        ..Default::default()
    };
    let request = pb::RedeemWithPasskeyRequest {
        invitation: invitation.to_owned(),
        name: name.to_owned(),
        registration: registration.into(),
        ..Default::default()
    };
    let response = accounts(h).redeem_with_passkey(request).await?;
    let cookie = refresh_cookie(response.headers()).unwrap_or_default();
    Ok((response.into_owned(), cookie))
}
