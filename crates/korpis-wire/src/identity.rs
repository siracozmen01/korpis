// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity provider calls on the wire (§5 of docs/08-identity.md).

use crate::{time, timestamp};
use korpis_core::extension::identity::{Assertion, Authorization, Evidence, Members, MembersAsked};
use korpis_core::extension::{CallOutcome, CallRequest};
use korpis_core::principal::SetName;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;

/// The wire form of a set.
#[must_use]
pub fn set_to_pb(s: &SetName) -> pb::ExternalSet {
    pb::ExternalSet { realm: s.realm.clone(), member_of: s.member_of.clone(), ..Default::default() }
}

/// A set from the wire.
#[must_use]
pub fn set_from_pb(s: &pb::ExternalSet) -> SetName {
    SetName { realm: s.realm.clone(), member_of: s.member_of.clone() }
}

/// An identity request as it goes down a provider's stream, if `r` is one.
#[must_use]
pub fn request_to_pb(r: &CallRequest) -> Option<Request> {
    Some(match r {
        CallRequest::VerifyMembers(a) => Request::VerifyMembers(Box::new(pb::VerifyMembers {
            set: a.set.as_ref().map(set_to_pb).into(),
            members: a.members.clone(),
            require_membership: a.require_membership.iter().map(set_to_pb).collect(),
            ..Default::default()
        })),
        CallRequest::Assert(e) => Request::Assert(Box::new(pb::Assert {
            evidence: e.body.clone(),
            headers: e.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            ..Default::default()
        })),
        CallRequest::Authorize(s) => Request::Authorize(Box::new(pb::Authorize {
            state: s.state.clone(),
            nonce: s.nonce.clone(),
            code_challenge: s.code_challenge.clone(),
            redirect_uri: s.redirect_uri.clone(),
            ..Default::default()
        })),
        _ => return None,
    })
}

/// An identity answer as a provider gave it, if `o` is one; whether it answers its call is the
/// domain's to judge.
#[must_use]
pub fn outcome_from_pb(o: &Outcome) -> Option<CallOutcome> {
    match o {
        Outcome::Authorization(a) => {
            Some(CallOutcome::Authorization(Authorization { url: a.url.clone() }))
        }
        Outcome::Members(m) => Some(CallOutcome::Members(Members {
            set_exists: m.set_exists,
            present: m.present.clone(),
            unknown: m.unknown.clone(),
        })),
        Outcome::Assertion(a) => {
            let at = a.at.as_option().and_then(|t| time("assertion.at", t).ok())?;
            Some(CallOutcome::Asserted(Assertion {
                user: a.user.clone(),
                sets: a.sets.iter().map(set_from_pb).collect(),
                presentation: a.presentation.clone(),
                at,
            }))
        }
        _ => None,
    }
}

/// The wire forms the SDK hands an implementation, and answers with.
#[must_use]
pub fn asked_from_pb(v: &pb::VerifyMembers) -> MembersAsked {
    MembersAsked {
        set: v.set.as_option().map(set_from_pb),
        members: v.members.clone(),
        require_membership: v.require_membership.iter().map(set_from_pb).collect(),
    }
}

/// Evidence from the wire.
#[must_use]
pub fn evidence_from_pb(a: &pb::Assert) -> Evidence {
    Evidence {
        body: a.evidence.clone(),
        headers: a.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    }
}

/// The wire form of an assertion.
#[must_use]
pub fn assertion_to_pb(a: &Assertion) -> pb::Assertion {
    pb::Assertion {
        user: a.user.clone(),
        sets: a.sets.iter().map(set_to_pb).collect(),
        presentation: a.presentation.clone(),
        at: Some(timestamp(a.at)).into(),
        ..Default::default()
    }
}
