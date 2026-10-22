// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a session begun at a portal may change of its account, and what the secrets it set
//! reach afterwards (§9.8 of docs/08-identity.md, Findings 131 and 133 of
//! docs/23-walkthroughs.md): a confined session changes the password while every grant the
//! account holds lies beneath the portal's organization, and not once one lies elsewhere; a
//! password and TOTP set at the portal sign in anywhere reaching beneath its organization alone,
//! a grant issued later elsewhere included; and a session they began registers no passkey at the
//! installation's own address.

use crate::domains::child;
use crate::panel::{access, accounts, refresh_cookie};
use crate::portal_client::{Portal, portal_of};
use crate::refusal::reason;
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_core::account::totp::{STEP_SECONDS, TotpSecret};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::sign_in_with_password_request::Second;
use std::time::{SystemTime, UNIX_EPOCH};

const PASSWORD: &str = "correct horse battery";

fn code(secret: &[u8], ahead: u64) -> String {
    let secret = TotpSecret::from_bytes(secret.try_into().unwrap());
    let step = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() / STEP_SECONDS;
    format!("{:06}", secret.code(step + ahead))
}

/// Issues `org.read` on `org` to `subject`, a new user when none; the response.
async fn granted(h: &Harness, key: &str, org: &str, user: Option<&str>) -> pb::IssueGrantResponse {
    let mut issued = pb::IssueGrantRequest {
        mutation: mutation(key).into(),
        actions: vec!["org.read".to_owned()],
        scope: org_scope(org).into(),
        ..Default::default()
    };
    match user {
        Some(u) => {
            let kind = pb::subject::Kind::User(u.to_owned());
            issued.subject = pb::Subject { kind: Some(kind), ..Default::default() }.into();
        }
        None => {
            issued.to_new_user =
                pb::NewUser { display_name: "Ada".into(), ..Default::default() }.into();
        }
    }
    h.grants(h.root()).issue_grant(issued).await.unwrap().into_owned()
}

/// Redeems `invitation` at `portal` with a password and TOTP; the account, the TOTP secret, and
/// an access token of the session the redemption began there.
async fn redeemed_at(portal: &Portal, invitation: &str) -> (String, Vec<u8>, String) {
    let asked = pb::OfferTotpRequest { name: "ada".into(), ..Default::default() };
    let offered = portal.call("korpis.v1.AccountService/OfferTotp", &asked, None).await;
    let (offer, _): (pb::OfferTotpResponse, _) = offered.unwrap();
    let redeem = pb::RedeemWithPasswordRequest {
        invitation: invitation.into(),
        name: "ada".into(),
        password: PASSWORD.into(),
        totp_secret: offer.secret.clone(),
        totp_code: code(&offer.secret, 0),
        ..Default::default()
    };
    let done = portal.call("korpis.v1.AccountService/RedeemWithPassword", &redeem, None).await;
    let (redeemed, headers): (pb::RedeemResponse, _) = done.unwrap();
    let cookie = refresh_cookie(&headers).unwrap();
    let exchange = pb::ExchangeRequest::default();
    let exchanged = portal.call("korpis.v1.SessionService/Exchange", &exchange, Some(&cookie));
    let (exchanged, _): (pb::ExchangeResponse, _) = exchanged.await.unwrap();
    (redeemed.account, offer.secret, exchanged.access_token)
}

fn profile(h: &Harness, bearer: &str) -> pb::ProfileServiceClient<HttpClient> {
    pb::ProfileServiceClient::new(h.http.clone(), h.config(Some(bearer)))
}

fn new_password(password: &str) -> pb::SetPasswordRequest {
    pb::SetPasswordRequest { password: password.into(), ..Default::default() }
}

#[tokio::test]
async fn secrets_set_at_a_portal_reach_beneath_its_organization_wherever_they_are_used() {
    let h = Harness::new().await;
    let reseller = child(&h, "pinecone").await;
    let portal = portal_of(&h, ("p", &reseller), ("pinecone.example", "panel.pinecone.example"));
    let portal = portal.await;
    let invitation = granted(&h, "invite", &reseller, None).await.invitation;
    let (account, secret, at_portal) = redeemed_at(&portal, &invitation).await;

    // While every grant lies beneath Pinecone, the portal's session changes the password.
    profile(&h, &at_portal).set_password(new_password(PASSWORD)).await.unwrap();
    granted(&h, "northwind", &h.root_org(), Some(&account)).await;
    let me = profile(&h, &at_portal);
    let refused = me.set_password(new_password("another horse battery")).await.unwrap_err();
    assert_eq!(reason(&refused), "session.portal_confined");

    // Signed in with them at the installation's own address, the account reaches Pinecone's
    // organization and nothing else, the grant issued after them included.
    let sign_in = pb::SignInWithPasswordRequest {
        name: "ada".into(),
        password: PASSWORD.into(),
        second: Some(Second::TotpCode(code(&secret, 1))),
        ..Default::default()
    };
    let signed_in = accounts(&h).sign_in_with_password(sign_in).await.unwrap();
    let elsewhere = access(&h, &refresh_cookie(signed_in.headers()).unwrap()).await;
    let orgs = h.orgs(h.config(Some(&elsewhere)));
    let get = |id: &str| pb::GetOrganizationRequest { id: id.into(), ..Default::default() };
    orgs.get_organization(get(&reseller)).await.unwrap();
    let above = orgs.get_organization(get(&h.root_org())).await.unwrap_err();
    assert_eq!(above.code, ErrorCode::NotFound, "Northwind's grant is not reached with them");

    // Nor do they buy a passkey that would reach further.
    let theirs = profile(&h, &elsewhere);
    let begin = theirs.begin_add_passkey(pb::BeginAddPasskeyRequest::default()).await;
    assert_eq!(reason(&begin.unwrap_err()), "session.portal_confined");
}

/// Signs `name` in at `portal` with `PASSWORD` and `second`; an access token the session gives.
async fn password_at(portal: &Portal, second: Second) -> String {
    let sign_in = pb::SignInWithPasswordRequest {
        name: "ada".into(),
        password: PASSWORD.into(),
        second: Some(second),
        ..Default::default()
    };
    let done = portal.call("korpis.v1.AccountService/SignInWithPassword", &sign_in, None).await;
    let (_, headers): (pb::SignInResponse, _) = done.unwrap();
    let cookie = refresh_cookie(&headers).unwrap();
    let exchange = pb::ExchangeRequest::default();
    let exchanged = portal.call("korpis.v1.SessionService/Exchange", &exchange, Some(&cookie));
    let (exchanged, _): (pb::ExchangeResponse, _) = exchanged.await.unwrap();
    exchanged.access_token
}

#[tokio::test]
async fn secrets_set_at_two_unrelated_portals_sign_in_nowhere_together() {
    let h = Harness::new().await;
    let (pinecone, northwind) = (child(&h, "pinecone").await, child(&h, "northwind").await);
    let at_p = portal_of(&h, ("p", &pinecone), ("pinecone.example", "panel.pinecone.example"));
    let at_n = portal_of(&h, ("n", &northwind), ("northwind.example", "panel.northwind.example"));
    let (at_p, at_n) = (at_p.await, at_n.await);

    // Ada redeems at the installation's own address, and then holds no grant at all.
    let issued = granted(&h, "invite", &h.root_org(), None).await;
    let accounts = accounts(&h);
    let offer = pb::OfferTotpRequest { name: "ada".into(), ..Default::default() };
    let first = accounts.offer_totp(offer).await.unwrap().into_owned().secret;
    let redeem = pb::RedeemWithPasswordRequest {
        invitation: issued.invitation,
        name: "ada".into(),
        password: PASSWORD.into(),
        totp_secret: first.clone(),
        totp_code: code(&first, 0),
        ..Default::default()
    };
    let redeemed = accounts.redeem_with_password(redeem).await.unwrap().into_owned();
    let grant = issued.grant.into_option().unwrap().id;
    let revoke =
        pb::RevokeGrantRequest { mutation: mutation("revoke").into(), grant, ..Default::default() };
    h.grants(h.root()).revoke_grant(revoke).await.unwrap();

    // With nothing held anywhere, Northwind's portal sees her enrol a new TOTP secret, and
    // Pinecone's sees her set her password: each change is allowed, and each is recorded.
    let at_northwind = password_at(&at_n, Second::TotpCode(code(&first, 1))).await;
    let second = TotpSecret::generate().unwrap();
    let enrol = pb::EnrolTotpRequest {
        secret: second.bytes().to_vec(),
        code: code(second.bytes(), 0),
        ..Default::default()
    };
    profile(&h, &at_northwind).enrol_totp(enrol).await.unwrap();
    let recovery = Second::RecoveryCode(redeemed.recovery_codes[0].clone());
    let at_pinecone = password_at(&at_p, recovery).await;
    profile(&h, &at_pinecone).set_password(new_password(PASSWORD)).await.unwrap();

    // Granted something later, she signs in with the two nowhere: one party may hold both
    // organizations, and nothing bounds what it would reach.
    granted(&h, "later", &h.root_org(), Some(&redeemed.account)).await;
    let guess = pb::SignInWithPasswordRequest {
        name: "ada".into(),
        password: PASSWORD.into(),
        second: Some(Second::TotpCode("000000".into())),
        ..Default::default()
    };
    let wrong = accounts.sign_in_with_password(guess).await.unwrap_err();
    assert_eq!(wrong.code, ErrorCode::Unauthenticated, "the password alone learns nothing");
    let sign_in = pb::SignInWithPasswordRequest {
        name: "ada".into(),
        password: PASSWORD.into(),
        second: Some(Second::TotpCode(code(second.bytes(), 1))),
        ..Default::default()
    };
    let refused = accounts.sign_in_with_password(sign_in).await.unwrap_err();
    assert_eq!(reason(&refused), "session.portal_confined");
}
