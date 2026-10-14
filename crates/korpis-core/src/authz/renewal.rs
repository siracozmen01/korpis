// SPDX-License-Identifier: AGPL-3.0-or-later

//! Renewing a session token (§6.1 of docs/08-identity.md): the gateway mints its successor for a
//! caller who presents it while it lives. A use of a use-limited grant is a redemption, and the
//! tokens that keep the session it opened alive are renewals of that redemption, not new uses;
//! what makes a mint a renewal is that it shows the live token it continues, which this reads.

use super::scope::{Kind, ObjectRef};
use super::token::{KeyRing, TokenError, chain_of};
use biscuit_auth::AuthorizerLimits;
use biscuit_auth::builder::{Term, date};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const LIMITS: AuthorizerLimits =
    AuthorizerLimits { max_facts: 1000, max_iterations: 100, max_time: Duration::from_millis(100) };

/// What a live session token says, as the control plane signed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renewed {
    /// Its holder's subject key.
    pub subject: String,
    /// Its grant chain, leaf first.
    pub chain: Vec<Uuid>,
    /// The actions it carries.
    pub actions: BTreeSet<String>,
    /// The one object it is for.
    pub scope: ObjectRef,
}

/// Reads `token` at `now`: it must verify against `keys` and not have expired. Only the
/// authority block is believed, so a block its holder appended can narrow what it may do and
/// never what it says it is.
pub fn renewed(token: &str, keys: &KeyRing, now: SystemTime) -> Result<Renewed, TokenError> {
    let biscuit = keys.parse(token)?;
    let mut params: HashMap<String, Term> = HashMap::new();
    params.insert("now".into(), date(&now));
    let mut authorizer = biscuit_auth::AuthorizerBuilder::new()
        .code_with_params("time({now});\nallow if subject($s);", params, HashMap::new())?
        .set_limits(LIMITS)
        .build(&biscuit)?;
    authorizer.authorize().map_err(|e| TokenError::Unauthorized(e.to_string()))?;
    let subject: Vec<(String,)> = authorizer.query("data($s) <- subject($s)")?;
    let [(subject,)] = subject.as_slice() else {
        return Err(TokenError::Invalid("a session token names one subject".into()));
    };
    let scope: Vec<(String, String)> = authorizer.query("data($k, $i) <- scope($k, $i)")?;
    let scope = match scope.as_slice() {
        [(k, i)] => {
            Kind::parse(k).zip(Uuid::parse_str(i).ok()).map(|(kind, id)| ObjectRef::new(kind, id))
        }
        _ => None,
    }
    .ok_or_else(|| TokenError::Invalid("a session token is for one object".into()))?;
    let actions: Vec<(String,)> = authorizer.query("data($a) <- right($a)")?;
    let chain = chain_of(&mut authorizer)?;
    Ok(Renewed {
        subject: subject.clone(),
        chain,
        actions: actions.into_iter().map(|(a,)| a).collect(),
        scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::link;
    use crate::authz::token::{Claims, SigningKey, mint};

    #[test]
    fn a_live_session_token_reads_back_and_nothing_else_does() {
        let (key, now) = (SigningKey::generate(1), SystemTime::now());
        let mut ring = KeyRing::default();
        ring.insert(1, &key.public_bytes()).unwrap();
        let claims = Claims {
            subject: "user:x".into(),
            chain: vec![Uuid::now_v7(), Uuid::now_v7()],
            actions: vec!["workload.console.read".into()],
            scope: ObjectRef::new(Kind::Workload, Uuid::now_v7()),
            expires_at: now + Duration::from_secs(120),
            source_cidr: Vec::new(),
        };
        let token = mint(&key, &claims).unwrap();
        let read = renewed(&token, &ring, now).unwrap();
        assert_eq!(
            (read.subject.as_str(), &read.chain, read.scope),
            ("user:x", &claims.chain, claims.scope)
        );
        assert_eq!(read.actions, BTreeSet::from(["workload.console.read".to_owned()]));
        assert!(renewed(&token, &ring, now + Duration::from_secs(121)).is_err(), "expired");
        let l = link::mint(&key, Uuid::now_v7(), now + Duration::from_secs(3600)).unwrap();
        assert!(
            renewed(&l[link::LINK_PREFIX.len()..], &ring, now).is_err(),
            "a link renews nothing"
        );
    }
}
