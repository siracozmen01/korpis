// SPDX-License-Identifier: AGPL-3.0-or-later

//! Providers' wire forms (§3.3 of docs/16-extensions.md): registrations,
//! `korpis.v1.ProviderRegistration`, and the calls sent down a provider's stream and the results
//! that come back up it.

use crate::extension::provider_type_to_pb;
use crate::{IdKind, timestamp, wire_id};
use korpis_core::extension::{CallFailure, CallOutcome, CallRequest, ProviderCall, Registration};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;

/// The wire form of a registration.
#[must_use]
pub fn registration_to_pb(r: &Registration) -> pb::ProviderRegistration {
    pb::ProviderRegistration {
        id: wire_id(IdKind::Registration, r.id),
        extension: wire_id(IdKind::Extension, r.extension),
        organization: wire_id(IdKind::Organization, r.organization),
        r#type: provider_type_to_pb(r.kind).into(),
        name: r.name.clone(),
        version: r.version.clone(),
        concurrency: r.concurrency,
        connected: timestamp(r.connected_at).into(),
        seen: timestamp(r.seen_at).into(),
        ..Default::default()
    }
}

/// A call as it goes down a provider's stream.
#[must_use]
pub fn call_to_pb(c: &ProviderCall) -> pb::ProviderCall {
    let request = match &c.request {
        CallRequest::Ping { nonce } => {
            Request::Ping(Box::new(pb::Ping { nonce: nonce.clone(), ..Default::default() }))
        }
    };
    pb::ProviderCall {
        call: wire_id(IdKind::ProviderCall, c.id),
        deadline: timestamp(c.deadline).into(),
        request: Some(request),
        ..Default::default()
    }
}

/// How a provider says a call ended. An answer of no recognisable kind is the provider's
/// failure, not the control plane's: it is recorded as such and is not retried.
#[must_use]
pub fn outcome_from_pb(r: &pb::CallResult) -> CallOutcome {
    match &r.outcome {
        Some(Outcome::Pong(p)) => CallOutcome::Pong { nonce: p.nonce.clone() },
        Some(Outcome::Error(e)) => CallOutcome::Failed(CallFailure {
            reason: if e.reason.is_empty() { "provider.failed".into() } else { e.reason.clone() },
            message: e.message.clone(),
            retryable: e.retryable,
        }),
        None => CallOutcome::Failed(CallFailure::of(
            "provider.malformed",
            "the provider answered with no outcome",
            false,
        )),
    }
}

/// A failed call as the API reports it.
#[must_use]
pub fn failure_to_pb(f: &CallFailure) -> pb::Error {
    pb::Error {
        code: pb::ErrorCode::ERROR_CODE_UNAVAILABLE.into(),
        reason: f.reason.clone(),
        message: f.message.clone(),
        retryable: f.retryable,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_without_an_outcome_is_the_providers_failure() {
        let empty = pb::CallResult::default();
        let CallOutcome::Failed(f) = outcome_from_pb(&empty) else { panic!("a failure") };
        assert_eq!((f.reason.as_str(), f.retryable), ("provider.malformed", false));
        let pong = pb::CallResult {
            outcome: Some(Outcome::Pong(Box::new(pb::Pong {
                nonce: "n".into(),
                ..Default::default()
            }))),
            ..Default::default()
        };
        assert_eq!(outcome_from_pb(&pong), CallOutcome::Pong { nonce: "n".into() });
    }
}
