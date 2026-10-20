// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity provider calls in the stored queue (§5 of docs/08-identity.md). Evidence is kept as
//! the person presented it, hex-encoded, only while its call is queued and recorded, like any
//! call; an assertion keeps its instant in milliseconds since the Unix epoch.

use super::keys::{hex, unhex};
use korpis_core::extension::identity::{
    Assertion, Authorization, Evidence, Members, MembersAsked, SignIn,
};
use korpis_core::extension::{CallOutcome, CallRequest};
use korpis_core::principal::SetName;
use serde_json::{Value, json};
use std::time::{Duration, UNIX_EPOCH};

fn sets_to(sets: &[SetName]) -> Value {
    sets.iter().map(|s| json!({ "realm": s.realm, "member_of": s.member_of })).collect()
}

fn set_from(v: &Value) -> Option<SetName> {
    Some(SetName {
        realm: v.get("realm")?.as_str()?.to_owned(),
        member_of: v.get("member_of")?.as_str()?.to_owned(),
    })
}

fn sets_from(v: Option<&Value>) -> Option<Vec<SetName>> {
    v?.as_array()?.iter().map(set_from).collect()
}

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    v?.as_array()?.iter().map(|s| s.as_str().map(str::to_owned)).collect()
}

/// An identity request to store, if `r` is one.
pub(crate) fn request_to(r: &CallRequest) -> Option<Value> {
    Some(match r {
        CallRequest::VerifyMembers(a) => json!({ "verify_members": {
            "set": a.set.as_ref().map(|s| json!({ "realm": s.realm, "member_of": s.member_of })),
            "members": a.members,
            "require_membership": sets_to(&a.require_membership),
        } }),
        CallRequest::Assert(e) => json!({ "assert": {
            "evidence": hex(&e.body), "headers": e.headers,
        } }),
        CallRequest::Authorize(s) => json!({ "authorize": {
            "state": s.state, "nonce": s.nonce, "code_challenge": s.code_challenge,
            "redirect_uri": s.redirect_uri,
        } }),
        _ => return None,
    })
}

/// A stored identity request, if `v` is one.
pub(crate) fn request_from(v: &Value) -> Option<CallRequest> {
    if let Some(m) = v.get("verify_members") {
        let set = match m.get("set") {
            None | Some(Value::Null) => None,
            Some(s) => Some(set_from(s)?),
        };
        return Some(CallRequest::VerifyMembers(MembersAsked {
            set,
            members: strings(m.get("members"))?,
            require_membership: sets_from(m.get("require_membership"))?,
        }));
    }
    if let Some(s) = v.get("authorize") {
        let text = |k: &str| s.get(k)?.as_str().map(str::to_owned);
        return Some(CallRequest::Authorize(SignIn {
            state: text("state")?,
            nonce: text("nonce")?,
            code_challenge: text("code_challenge")?,
            redirect_uri: text("redirect_uri")?,
        }));
    }
    let a = v.get("assert")?;
    let headers = a
        .get("headers")?
        .as_object()?
        .iter()
        .map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect::<Option<_>>()?;
    Some(CallRequest::Assert(Evidence { body: unhex(a, "evidence")?, headers }))
}

/// An identity outcome to store, if `o` is one.
pub(crate) fn outcome_to(o: &CallOutcome) -> Option<Value> {
    match o {
        CallOutcome::Members(m) => {
            let members =
                json!({ "set_exists": m.set_exists, "present": m.present, "unknown": m.unknown });
            Some(json!({ "members": members }))
        }
        CallOutcome::Authorization(a) => Some(json!({ "authorization": { "url": a.url } })),
        CallOutcome::Asserted(a) => {
            let ms = a.at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
            Some(json!({ "asserted": {
                "user": a.user, "sets": sets_to(&a.sets), "presentation": a.presentation,
                "at_ms": u64::try_from(ms).unwrap_or(u64::MAX),
            } }))
        }
        _ => None,
    }
}

/// A stored identity outcome, if `v` is one.
pub(crate) fn outcome_from(v: &Value) -> Option<CallOutcome> {
    if let Some(m) = v.get("members") {
        return Some(CallOutcome::Members(Members {
            set_exists: m.get("set_exists")?.as_bool()?,
            present: strings(m.get("present"))?,
            // Answers stored before a provider could say it cannot tell named nobody so.
            unknown: m.get("unknown").map_or(Some(Vec::new()), |u| strings(Some(u)))?,
        }));
    }
    if let Some(a) = v.get("authorization") {
        let url = a.get("url")?.as_str()?.to_owned();
        return Some(CallOutcome::Authorization(Authorization { url }));
    }
    let a = v.get("asserted")?;
    Some(CallOutcome::Asserted(Assertion {
        user: a.get("user")?.as_str()?.to_owned(),
        sets: sets_from(a.get("sets"))?,
        presentation: a.get("presentation")?.as_str()?.to_owned(),
        at: UNIX_EPOCH + Duration::from_millis(a.get("at_ms")?.as_u64()?),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn moderators() -> SetName {
        SetName { realm: "456".into(), member_of: "moderator".into() }
    }

    #[test]
    fn calls_and_answers_read_back_as_they_were_stored() {
        let asks = [
            CallRequest::VerifyMembers(MembersAsked {
                set: Some(moderators()),
                members: vec!["1".into(), "2".into()],
                require_membership: vec![moderators()],
            }),
            CallRequest::VerifyMembers(MembersAsked {
                set: None,
                members: vec!["1".into()],
                require_membership: Vec::new(),
            }),
            CallRequest::Assert(Evidence {
                body: b"{\"type\":2}".to_vec(),
                headers: BTreeMap::from([("x-signature-ed25519".into(), "ab".into())]),
            }),
            CallRequest::Authorize(SignIn {
                state: "s".into(),
                nonce: "n".into(),
                code_challenge: "c".into(),
                redirect_uri: "https://panel.example/sign-in/callback".into(),
            }),
        ];
        for r in asks {
            assert_eq!(request_from(&request_to(&r).unwrap()), Some(r));
        }
        let answers = [
            CallOutcome::Authorization(Authorization { url: "https://idp.example/auth".into() }),
            CallOutcome::Members(Members {
                set_exists: true,
                present: vec!["2".into()],
                unknown: vec!["3".into()],
            }),
            CallOutcome::Asserted(Assertion {
                user: "1".into(),
                sets: vec![moderators()],
                presentation: "interaction-9".into(),
                at: UNIX_EPOCH + Duration::from_millis(1_760_000_000_123),
            }),
        ];
        for o in answers {
            assert_eq!(outcome_from(&outcome_to(&o).unwrap()), Some(o));
        }
    }
}
