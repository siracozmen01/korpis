// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in through an identity provider, begun (§9.7 of docs/08-identity.md), against a real
//! control plane and a provider written with the SDK: the sign-in page lists the root
//! organization's providers that sign people in, without a credential; a sign-in begun is sent
//! where the provider says, carrying a state, a nonce, the challenge, and the panel's callback;
//! and the state and nonce are not left in the stored call.

use crate::bindings::identity_document;
use crate::extensions::push_document;
use crate::providers::installed_from;
use crate::support::Harness;
use connectrpc::ErrorCode;
use connectrpc::client::HttpClient;
use korpis_extension::identity_provider::{
    Answer, Asked, Asserted, Failure, Identity, IdentityProvider, Set, SignInAsked,
};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// A challenge, RFC 7636's own example.
pub const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// An identity provider that sends people to `https://idp.example/authorize`, with what the
/// control plane began the sign-in with in the query. A code it asserts is
/// `<user>|<nonce>|<set>,<set>…|<presentation>`, and only beside the nonce it names, a verifier,
/// and the panel's callback, as an OIDC provider redeems one; `eve` is nobody. Asked about a
/// set's members, it has every set, and everyone in it but those in [`GONE`].
pub struct Issuer;

/// The people who have left every set the issuer has; everyone else asked about is in them.
pub static GONE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Refuses what was presented.
fn unverified(why: &str) -> Failure {
    Failure { reason: "identity.unverified".into(), message: why.into(), retryable: false }
}

/// The person a code names, if `headers` are what a sign-in sends beside it.
fn asserted(code: &str, headers: &BTreeMap<String, String>) -> Result<Asserted, Failure> {
    let [user, nonce, sets, presentation] = code.split('|').collect::<Vec<_>>()[..] else {
        return Err(unverified("not a code"));
    };
    let header = |k: &str| headers.get(k).map_or("", String::as_str);
    if user == "eve" || header("nonce") != nonce || header("code_verifier").is_empty() {
        return Err(unverified("not this sign-in's code"));
    }
    if !header("redirect_uri").ends_with("/sign-in/callback") {
        return Err(unverified("redeemed for another redirect"));
    }
    let sets = sets.split(',').filter(|s| !s.is_empty());
    Ok(Asserted {
        user: user.to_owned(),
        sets: sets.map(|s| Set { realm: "groups".into(), member_of: s.into() }).collect(),
        presentation: presentation.to_owned(),
        at: SystemTime::now(),
    })
}

impl IdentityProvider for Issuer {
    fn verify(&self, asked: Asked) -> impl Future<Output = Result<Answer, Failure>> + Send {
        let gone = GONE.lock().map(|g| g.clone()).unwrap_or_default();
        let present = asked.members.into_iter().filter(|m| !gone.contains(m)).collect();
        std::future::ready(Ok(Answer { set_exists: true, present, unknown: Vec::new() }))
    }

    fn assert(
        &self,
        evidence: Vec<u8>,
        headers: BTreeMap<String, String>,
    ) -> impl Future<Output = Result<Asserted, Failure>> + Send {
        std::future::ready(asserted(&String::from_utf8(evidence).unwrap_or_default(), &headers))
    }

    fn authorize(&self, a: SignInAsked) -> impl Future<Output = Result<String, Failure>> + Send {
        std::future::ready(Ok(format!(
            "https://idp.example/authorize?state={}&nonce={}&code_challenge={}&redirect_uri={}",
            a.state, a.nonce, a.code_challenge, a.redirect_uri
        )))
    }
}

/// The issuer, installed as the `n`th node's extension and registered as `name`, signing people
/// in if `sign_in`; while the handle runs.
pub async fn issuing(h: &Harness, n: u8, name: &str, sign_in: bool) -> tokio::task::JoinHandle<()> {
    let document = identity_document()
        .replace("name: corp", &format!("name: {name}"))
        .replace("name: cloudflare-dns", &format!("name: issuer-{name}"));
    let x = installed_from(h, n, &format!("issuer-{n}"), push_document(&document).await).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_IDENTITY,
        name: name.into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in,
        fencing: pb::Fencing::FENCING_UNSPECIFIED,
    };
    let provider = Arc::new(Provider::new(connection, registration, Identity(Issuer)));
    let mut registered = provider.registered();
    let running = tokio::spawn(async move {
        let _ = provider.run().await;
        drop(x);
    });
    tokio::time::timeout(Duration::from_secs(10), registered.wait_for(Option::is_some))
        .await
        .expect("registered within ten seconds")
        .unwrap();
    running
}

/// The session service, called without a credential.
pub fn anonymous(h: &Harness) -> pb::SessionServiceClient<HttpClient> {
    pb::SessionServiceClient::new(h.http.clone(), h.config(None))
}

pub fn begin(org: &str, provider: &str, challenge: &str) -> pb::BeginExternalSignInRequest {
    pb::BeginExternalSignInRequest {
        organization: org.to_owned(),
        provider: provider.to_owned(),
        code_challenge: challenge.to_owned(),
        ..Default::default()
    }
}

/// The value of `key` in `url`'s query.
pub fn query<'a>(url: &'a str, key: &str) -> &'a str {
    let q = url.split_once('?').map_or("", |(_, q)| q);
    q.split('&').find_map(|kv| kv.strip_prefix(&format!("{key}="))).unwrap_or("")
}

#[tokio::test]
async fn a_sign_in_is_begun_without_a_credential_and_sent_where_its_provider_says() {
    let h = Harness::new().await;
    let _corp = issuing(&h, 61, "corp", true).await;
    let _directory = issuing(&h, 62, "directory", false).await;
    let org = h.root_org();

    let listed = anonymous(&h)
        .list_sign_in_providers(pb::ListSignInProvidersRequest::default())
        .await
        .unwrap();
    let names: Vec<_> = listed.into_owned().providers.into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["corp"], "a provider that signs nobody in is not offered");

    let begun = anonymous(&h).begin_external_sign_in(begin(&org, "corp", CHALLENGE)).await;
    let begun = begun.unwrap().into_owned();
    let url = begun.url.clone();
    assert!(url.starts_with("https://idp.example/authorize?"), "{url}");
    assert_eq!(query(&url, "state").len(), 43, "256 bits: {url}");
    assert_eq!(query(&url, "nonce").len(), 43);
    assert_ne!(query(&url, "state"), query(&url, "nonce"));
    assert_eq!(query(&url, "code_challenge"), CHALLENGE);
    assert!(query(&url, "redirect_uri").ends_with("/sign-in/callback"), "{url}");
    assert!(begun.expires_at.as_option().is_some());

    // The call that carried them keeps neither.
    let store = h.db.store().await.unwrap();
    let kept: Vec<String> =
        sqlx::query_scalar("SELECT request::text FROM provider_call WHERE request ? 'authorize'")
            .fetch_all(store.pool())
            .await
            .unwrap();
    assert_eq!(kept.len(), 1);
    assert!(!kept[0].contains(query(&url, "state")), "{}", kept[0]);
    assert!(!kept[0].contains(query(&url, "nonce")), "{}", kept[0]);
}

#[tokio::test]
async fn a_sign_in_is_refused_a_bad_challenge_and_a_provider_that_signs_nobody_in() {
    let h = Harness::new().await;
    let _directory = issuing(&h, 63, "directory", false).await;
    let org = h.root_org();
    let sessions = anonymous(&h);

    let plain = sessions.begin_external_sign_in(begin(&org, "directory", "plain")).await;
    assert_eq!(plain.unwrap_err().code, ErrorCode::InvalidArgument, "S256 only");
    let refused = sessions.begin_external_sign_in(begin(&org, "directory", CHALLENGE)).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::NotFound, "it registered without sign_in");
    let nobody = sessions.begin_external_sign_in(begin(&org, "nobody", CHALLENGE)).await;
    assert_eq!(nobody.unwrap_err().code, ErrorCode::NotFound);
}
