// SPDX-License-Identifier: AGPL-3.0-or-later

//! Providers' wire forms (§3.3 of docs/16-extensions.md): registrations,
//! `korpis.v1.ProviderRegistration`, and the calls sent down a provider's stream and the results
//! that come back up it.

use crate::dns::record_to_pb;
use crate::extension::provider_type_to_pb;
use crate::step::{run_step_to_pb, step_result_from_pb};
use crate::{IdKind, duration, timestamp, wire_id};
use korpis_core::extension::{
    Breaker, CallFailure, CallOutcome, CallRequest, Measured, ProviderCall, ProviderType,
    Registration,
};
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

/// The wire form of a provider's health.
#[must_use]
pub fn health_to_pb(
    kind: ProviderType,
    name: &str,
    b: &Breaker,
    m: &Measured,
) -> pb::ProviderHealth {
    let (breaker, until) = match b {
        Breaker::Closed => (pb::BreakerState::BREAKER_STATE_CLOSED, None),
        Breaker::Open { until, .. } => (pb::BreakerState::BREAKER_STATE_OPEN, Some(*until)),
        Breaker::HalfOpen { until, .. } => {
            (pb::BreakerState::BREAKER_STATE_HALF_OPEN, Some(*until))
        }
    };
    pb::ProviderHealth {
        r#type: provider_type_to_pb(kind).into(),
        name: name.to_owned(),
        breaker: breaker.into(),
        until: until.map(timestamp).into(),
        calls: u32::try_from(m.calls).unwrap_or(u32::MAX),
        failed: u32::try_from(m.failed).unwrap_or(u32::MAX),
        median_latency: duration(m.median_elapsed).into(),
        median_deadline_permille: m.median_share,
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
        CallRequest::GrantAccess { .. }
        | CallRequest::ListObjects { .. }
        | CallRequest::DeleteObjects { .. } => match crate::backup::request_to_pb(&c.request) {
            Some(r) => r,
            None => Request::Ping(Box::default()),
        },
        CallRequest::GenerateKey { .. } | CallRequest::UnwrapKey { .. } => {
            match crate::keys::request_to_pb(&c.request) {
                Some(r) => r,
                None => Request::Ping(Box::default()),
            }
        }
        CallRequest::VerifyMembers(_) | CallRequest::Assert(_) => {
            match crate::identity::request_to_pb(&c.request) {
                Some(r) => r,
                None => Request::Ping(Box::default()),
            }
        }
        CallRequest::RunStep { step, arguments, workload, platform } => {
            Request::RunStep(Box::new(run_step_to_pb(step, arguments, *workload, platform)))
        }
        CallRequest::ApplyRecords { owner, records } => {
            Request::ApplyRecords(Box::new(pb::ApplyRecords {
                owner: owner.clone(),
                records: records.iter().map(record_to_pb).collect(),
                ..Default::default()
            }))
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
        Some(Outcome::StepResult(r)) => CallOutcome::StepResult(step_result_from_pb(r)),
        Some(
            o @ (Outcome::ObjectAccess(_) | Outcome::ObjectList(_) | Outcome::ObjectsDeleted(_)),
        ) => crate::backup::outcome_from_pb(o)
            .unwrap_or(CallOutcome::Failed(CallFailure::malformed())),
        Some(o @ (Outcome::Members(_) | Outcome::Assertion(_))) => {
            crate::identity::outcome_from_pb(o)
                .unwrap_or(CallOutcome::Failed(CallFailure::malformed()))
        }
        Some(o @ Outcome::KeyMaterial(_)) => {
            crate::keys::outcome_from_pb(o).unwrap_or(CallOutcome::Failed(CallFailure::malformed()))
        }
        Some(Outcome::RecordsApplied(a)) => CallOutcome::RecordsApplied {
            created: a.created,
            updated: a.updated,
            deleted: a.deleted,
        },
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
