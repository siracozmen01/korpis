// SPDX-License-Identifier: Apache-2.0

//! Writing an identity provider (§5 of docs/08-identity.md): an OIDC issuer, a SAML identity
//! provider, a chat platform. It answers two calls, over its own stream, which is what makes its
//! word its own: whether a set it vouches for still is what it was and which of some people are
//! still in it (re-verification, §5.1), and who presented a piece of evidence and which of its
//! sets they are in (§5.2). Whatever it says is believed for its own subjects and nobody else's,
//! so it says nothing it has not verified: a signature checked, a timestamp within five minutes,
//! a presentation it names so the control plane accepts it once. Requires the
//! `provider.identity` capability.

pub use crate::provider::Failure;
use crate::provider::{Handler, Outcome, Unsupported};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::collections::BTreeMap;
use std::future::Future;
use std::time::{SystemTime, UNIX_EPOCH};

/// A set at the provider: where it lives, never containing `:`, and which set it is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Set {
    /// A guild, the claim or attribute naming groups.
    pub realm: String,
    /// The set within it.
    pub member_of: String,
}

/// A re-verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The set; `None` asks only whether the people are still known.
    pub set: Option<Set>,
    /// The people, by their identifier here.
    pub members: Vec<String>,
    /// Other sets each must also be in.
    pub require_membership: Vec<Set>,
}

/// The answer to an [`Asked`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    /// For a set: it exists and is the set it was.
    pub set_exists: bool,
    /// The people confirmed, only of those asked about.
    pub present: Vec<String>,
}

/// Who presented a piece of evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asserted {
    /// The person, by their identifier here.
    pub user: String,
    /// The sets of this provider they are in.
    pub sets: Vec<Set>,
    /// This one presentation's identifier, which the control plane accepts once.
    pub presentation: String,
    /// When it was presented.
    pub at: SystemTime,
}

/// An identity provider.
pub trait IdentityProvider: Send + Sync + 'static {
    /// Answers a re-verification.
    fn verify(&self, asked: Asked) -> impl Future<Output = Result<Answer, Failure>> + Send;

    /// Says who presented `evidence`, received with `headers`, or refuses it.
    fn assert(
        &self,
        evidence: Vec<u8>,
        headers: BTreeMap<String, String>,
    ) -> impl Future<Output = Result<Asserted, Failure>> + Send;
}

/// Serves an [`IdentityProvider`] as a provider's [`Handler`].
#[derive(Debug, Clone)]
pub struct Identity<P>(pub P);

fn set(s: &pb::ExternalSet) -> Set {
    Set { realm: s.realm.clone(), member_of: s.member_of.clone() }
}

fn set_to_pb(s: &Set) -> pb::ExternalSet {
    pb::ExternalSet { realm: s.realm.clone(), member_of: s.member_of.clone(), ..Default::default() }
}

fn assertion(a: &Asserted) -> Outcome {
    let since = a.at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let at = buffa_types::google::protobuf::Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    };
    Outcome::Assertion(Box::new(pb::Assertion {
        user: a.user.clone(),
        sets: a.sets.iter().map(set_to_pb).collect(),
        presentation: a.presentation.clone(),
        at: Some(at).into(),
        ..Default::default()
    }))
}

impl<P: IdentityProvider> Handler for Identity<P> {
    async fn call(&self, call: pb::ProviderCall) -> Outcome {
        let answered = match call.request {
            Some(Request::VerifyMembers(v)) => {
                let asked = Asked {
                    set: v.set.as_option().map(set),
                    members: v.members.clone(),
                    require_membership: v.require_membership.iter().map(set).collect(),
                };
                self.0.verify(asked).await.map(|a| {
                    Outcome::Members(Box::new(pb::Members {
                        set_exists: a.set_exists,
                        present: a.present,
                        ..Default::default()
                    }))
                })
            }
            Some(Request::Assert(a)) => {
                let headers = a.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                self.0.assert(a.evidence.clone(), headers).await.map(|a| assertion(&a))
            }
            _ => return Unsupported.call(call).await,
        };
        answered.unwrap_or_else(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider whose only set is `guild/mods`, holding `ann`, and whose evidence is a name.
    struct Guild;

    impl IdentityProvider for Guild {
        fn verify(&self, asked: Asked) -> impl Future<Output = Result<Answer, Failure>> + Send {
            let mods = Set { realm: "guild".into(), member_of: "mods".into() };
            let present = asked.members.into_iter().filter(|m| m == "ann").collect();
            std::future::ready(Ok(Answer { set_exists: asked.set == Some(mods), present }))
        }

        fn assert(
            &self,
            evidence: Vec<u8>,
            _: BTreeMap<String, String>,
        ) -> impl Future<Output = Result<Asserted, Failure>> + Send {
            std::future::ready(match String::from_utf8(evidence) {
                Ok(user) if user == "ann" => Ok(Asserted {
                    user,
                    sets: vec![Set { realm: "guild".into(), member_of: "mods".into() }],
                    presentation: "p-1".into(),
                    at: UNIX_EPOCH,
                }),
                _ => Err(Failure {
                    reason: "identity.unverified".into(),
                    message: "no such person".into(),
                    retryable: false,
                }),
            })
        }
    }

    fn call(request: Request) -> pb::ProviderCall {
        pb::ProviderCall { request: Some(request), ..Default::default() }
    }

    #[tokio::test]
    async fn a_re_verification_and_an_assertion_are_answered_in_the_contract_s_terms() {
        let verify = pb::VerifyMembers {
            set: Some(pb::ExternalSet {
                realm: "guild".into(),
                member_of: "mods".into(),
                ..Default::default()
            })
            .into(),
            members: vec!["ann".into(), "bob".into()],
            ..Default::default()
        };
        let Outcome::Members(m) =
            Identity(Guild).call(call(Request::VerifyMembers(Box::new(verify)))).await
        else {
            panic!("no members");
        };
        assert!(m.set_exists);
        assert_eq!(m.present, ["ann"]);
        let assert = pb::Assert { evidence: b"ann".to_vec(), ..Default::default() };
        let Outcome::Assertion(a) =
            Identity(Guild).call(call(Request::Assert(Box::new(assert)))).await
        else {
            panic!("no assertion");
        };
        assert_eq!((a.user.as_str(), a.presentation.as_str()), ("ann", "p-1"));
        let refused = pb::Assert { evidence: b"eve".to_vec(), ..Default::default() };
        let Outcome::Error(e) =
            Identity(Guild).call(call(Request::Assert(Box::new(refused)))).await
        else {
            panic!("an unverified presentation is refused");
        };
        assert_eq!(e.reason, "identity.unverified");
    }
}
