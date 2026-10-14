// SPDX-License-Identifier: AGPL-3.0-or-later

//! Key management calls on the wire (§5.5 of docs/06-storage.md).

use korpis_core::extension::{CallOutcome, CallRequest};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;

/// A key management request as it goes down a provider's stream, if `r` is one.
#[must_use]
pub fn request_to_pb(r: &CallRequest) -> Option<Request> {
    Some(match r {
        CallRequest::GenerateKey { key, context, recipient } => {
            Request::GenerateKey(Box::new(pb::GenerateKey {
                key: key.clone(),
                context: context.clone(),
                recipient: recipient.clone(),
                ..Default::default()
            }))
        }
        CallRequest::UnwrapKey { key, context, recipient, wrapped } => {
            Request::UnwrapKey(Box::new(pb::UnwrapKey {
                key: key.clone(),
                context: context.clone(),
                recipient: recipient.clone(),
                wrapped: wrapped.clone(),
                ..Default::default()
            }))
        }
        _ => return None,
    })
}

/// A key management answer as a provider gave it, if `o` is one; whether it answers its call
/// is the domain's to judge.
#[must_use]
pub fn outcome_from_pb(o: &Outcome) -> Option<CallOutcome> {
    match o {
        Outcome::KeyMaterial(k) => {
            Some(CallOutcome::KeyMaterial { wrapped: k.wrapped.clone(), sealed: k.sealed.clone() })
        }
        _ => None,
    }
}
