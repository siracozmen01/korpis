// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a provider call's request and outcome are stored: one JSON object with one key naming
//! the variant, so a type added later is a key added, never a column.

use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::extension::{CallFailure, CallOutcome, CallRequest};
use serde_json::{Value, json};

fn corrupt(what: &str, v: &Value) -> StoreError {
    StoreError::Corrupt(format!("provider call {what}: {v}"))
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A request to store.
pub(crate) fn request_to(r: &CallRequest) -> Value {
    match r {
        CallRequest::Ping { nonce } => json!({ "ping": { "nonce": nonce } }),
    }
}

/// A stored request.
pub(crate) fn request_from(v: &Value) -> StoreResult<CallRequest> {
    let ping = v.get("ping").ok_or_else(|| corrupt("request", v))?;
    let nonce = text(ping, "nonce").ok_or_else(|| corrupt("request", v))?;
    Ok(CallRequest::Ping { nonce })
}

/// An outcome to store.
pub(crate) fn outcome_to(o: &CallOutcome) -> Value {
    match o {
        CallOutcome::Pong { nonce } => json!({ "pong": { "nonce": nonce } }),
        CallOutcome::Failed(f) => json!({
            "failed": { "reason": f.reason, "message": f.message, "retryable": f.retryable }
        }),
    }
}

/// A stored outcome.
pub(crate) fn outcome_from(v: &Value) -> StoreResult<CallOutcome> {
    if let Some(pong) = v.get("pong") {
        let nonce = text(pong, "nonce").ok_or_else(|| corrupt("outcome", v))?;
        return Ok(CallOutcome::Pong { nonce });
    }
    let f = v.get("failed").ok_or_else(|| corrupt("outcome", v))?;
    let reason = text(f, "reason").ok_or_else(|| corrupt("outcome", v))?;
    let message = text(f, "message").unwrap_or_default();
    let retryable = f.get("retryable").and_then(Value::as_bool).unwrap_or(false);
    Ok(CallOutcome::Failed(CallFailure { reason, message, retryable }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_survives_the_round_trip() {
        let r = CallRequest::Ping { nonce: "n1".into() };
        assert_eq!(request_from(&request_to(&r)).unwrap(), r);
        for o in [
            CallOutcome::Pong { nonce: "n1".into() },
            CallOutcome::Failed(CallFailure::deadline()),
            CallOutcome::Failed(CallFailure::unknown()),
        ] {
            assert_eq!(outcome_from(&outcome_to(&o)).unwrap(), o);
        }
        assert!(outcome_from(&json!({ "other": {} })).is_err());
    }
}
