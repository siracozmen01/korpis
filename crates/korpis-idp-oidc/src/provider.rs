// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider's three answers (§5.3 and §9.7 of docs/08-identity.md): where to send a person
//! to sign in, who returned with a code and which groups they are in, and which of some people
//! are still in a group, asked of the issuer with each one's refresh token. It answers that it
//! cannot tell of anyone it holds no current evidence of, and never that they left.

use crate::claims::{self, Expected, Person};
use crate::config::Settings;
use crate::issuer::{Grant, Issuer, Refusal};
use crate::refresh::{Refreshed, Refreshes, Standing};
use futures::stream::{self, StreamExt as _};
use korpis_extension::identity_provider::{
    Answer, Asked, Asserted, Failure, IdentityProvider, Set, SignInAsked,
};
use ring::digest::{SHA256, digest};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::SystemTime;

/// How many people one re-verification refreshes at once, so a set of hundreds does not arrive
/// at the issuer as hundreds of requests together.
const AT_ONCE: usize = 16;

/// An OpenID Connect issuer as an identity provider.
#[derive(Debug)]
pub struct Oidc {
    issuer: Issuer,
    refreshes: Refreshes,
}

fn failure(r: Refusal) -> Failure {
    match r {
        Refusal::Refused(message) => {
            Failure { reason: "identity.unverified".into(), message, retryable: false }
        }
        Refusal::Unreachable(message) => {
            Failure { reason: "identity.unreachable".into(), message, retryable: true }
        }
    }
}

fn presentation(code: &str) -> String {
    let mut hex = String::with_capacity(64);
    for b in digest(&SHA256, code.as_bytes()).as_ref() {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

impl Oidc {
    /// The issuer `settings` names, reached with `http`.
    #[must_use]
    pub fn new(http: reqwest::Client, settings: Settings) -> Self {
        let refreshes = Refreshes::new(settings.state_dir.clone());
        Self { issuer: Issuer::new(http, settings), refreshes }
    }

    fn settings(&self) -> &Settings {
        self.issuer.settings()
    }

    /// The person `token` names, once it verifies and says what `nonce` asks.
    async fn person(&self, token: &str, nonce: Option<&str>) -> Result<Person, Refusal> {
        let claims = self.issuer.verified(token).await?;
        let s = self.settings();
        let expected = Expected {
            issuer: &s.issuer,
            client_id: &s.client_id,
            nonce,
            groups: &s.groups_claim,
            now: SystemTime::now(),
        };
        claims::validated(&claims, &expected).map_err(Refusal::Refused)
    }

    /// One refresh of `sub`'s `token`, and the groups the ID token it answered names.
    async fn refresh(&self, sub: &str, token: String) -> Refreshed {
        match self.issuer.redeem(Grant::Refresh(&token)).await {
            Ok(tokens) => {
                let groups = match tokens.id_token {
                    Some(t) => match self.person(&t, None).await {
                        Ok(p) if p.sub == sub => Some(p.groups),
                        _ => return Refreshed::Unreachable,
                    },
                    None => None,
                };
                Refreshed::Answered { groups, token: tokens.refresh_token }
            }
            Err(Refusal::Refused(_)) => Refreshed::Refused,
            Err(Refusal::Unreachable(_)) => Refreshed::Unreachable,
        }
    }

    /// What is known of `sub` now, by one refresh at most.
    async fn standing(&self, sub: String) -> Standing {
        self.refreshes.standing(&sub, |token| self.refresh(&sub, token)).await
    }

    /// Whether `groups` holds `set` and every one of `required`, all of this provider's realm.
    fn holds(&self, groups: &[String], set: &Set, required: &[Set]) -> bool {
        let realm = &self.settings().groups_claim;
        std::iter::once(set)
            .chain(required)
            .all(|s| &s.realm == realm && groups.contains(&s.member_of))
    }
}

impl IdentityProvider for Oidc {
    async fn verify(&self, asked: Asked) -> Result<Answer, Failure> {
        let realm = &self.settings().groups_claim;
        let set_exists = asked.set.as_ref().is_some_and(|s| &s.realm == realm);
        let standings: Vec<Standing> = stream::iter(asked.members.clone())
            .map(|m| self.standing(m))
            .buffered(AT_ONCE)
            .collect()
            .await;
        let mut answer = Answer { set_exists, ..Answer::default() };
        for (member, standing) in asked.members.into_iter().zip(standings) {
            let present = match (&asked.set, standing) {
                (_, Standing::Unknown) | (Some(_), Standing::Known(None)) => {
                    answer.unknown.push(member);
                    continue;
                }
                (_, Standing::Gone) => false,
                (None, Standing::Known(_)) => true,
                (Some(set), Standing::Known(Some(groups))) => {
                    set_exists && self.holds(&groups, set, &asked.require_membership)
                }
            };
            if present {
                answer.present.push(member);
            }
        }
        Ok(answer)
    }

    async fn assert(
        &self,
        evidence: Vec<u8>,
        headers: BTreeMap<String, String>,
    ) -> Result<Asserted, Failure> {
        let refused = |m: &str| failure(Refusal::Refused(m.to_owned()));
        let code = String::from_utf8(evidence).map_err(|_| refused("the code is not text"))?;
        let header = |k: &str| headers.get(k).map(String::as_str).ok_or_else(|| refused(k));
        let (verifier, redirect_uri, nonce) =
            (header("code_verifier")?, header("redirect_uri")?, header("nonce")?);
        let grant = Grant::Code { code: &code, verifier, redirect_uri };
        let tokens = self.issuer.redeem(grant).await.map_err(failure)?;
        let token = tokens.id_token.ok_or_else(|| refused("the issuer answered no ID token"))?;
        let person = self.person(&token, Some(nonce)).await.map_err(failure)?;
        if let Some(r) = tokens.refresh_token
            && let Err(e) = self.refreshes.keep(&person.sub, &r).await
        {
            tracing::warn!(error = %e, "a refresh token could not be kept");
        }
        let realm = &self.settings().groups_claim;
        Ok(Asserted {
            sets: person
                .groups
                .iter()
                .map(|g| Set { realm: realm.clone(), member_of: g.clone() })
                .collect(),
            user: person.sub,
            presentation: presentation(&code),
            at: person.issued_at,
        })
    }

    async fn authorize(&self, asked: SignInAsked) -> Result<String, Failure> {
        let mut url = self.issuer.authorization_endpoint().await.map_err(failure)?;
        let s = self.settings();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &s.client_id)
            .append_pair("redirect_uri", &asked.redirect_uri)
            .append_pair("scope", &s.scopes.join(" "))
            .append_pair("state", &asked.state)
            .append_pair("nonce", &asked.nonce)
            .append_pair("code_challenge", &asked.code_challenge)
            .append_pair("code_challenge_method", "S256");
        // OIDC Core §11: offline access is asked with consent, or an issuer may drop it.
        if s.scopes.iter().any(|scope| scope == "offline_access") {
            url.query_pairs_mut().append_pair("prompt", "consent");
        }
        Ok(url.into())
    }
}
