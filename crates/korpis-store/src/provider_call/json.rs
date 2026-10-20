// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a provider call's request and outcome are stored: one JSON object with one key naming
//! the variant, so a type added later is a key added, never a column.

use super::value::{map_from, map_to};
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::dns::{DnsRecord, RecordType};
use korpis_core::extension::{
    CallFailure, CallOutcome, CallRequest, Platform, StepArtifact, StepResult,
};
use serde_json::{Value, json};
use uuid::Uuid;

fn corrupt(what: &str, v: &Value) -> StoreError {
    StoreError::Corrupt(format!("provider call {what}: {v}"))
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn record_to(r: &DnsRecord) -> Value {
    json!({ "name": r.name, "type": r.kind.as_str(), "ttl": r.ttl, "values": r.values })
}

fn record_from(v: &Value) -> Option<DnsRecord> {
    let kind =
        [RecordType::A, RecordType::Aaaa, RecordType::Cname, RecordType::Srv, RecordType::Txt]
            .into_iter()
            .find(|k| v.get("type").and_then(Value::as_str) == Some(k.as_str()))?;
    let values = v.get("values")?.as_array()?;
    Some(DnsRecord {
        name: text(v, "name")?,
        kind,
        ttl: u32::try_from(v.get("ttl")?.as_u64()?).ok()?,
        values: values.iter().map(|x| x.as_str().map(str::to_owned)).collect::<Option<_>>()?,
    })
}

fn run_step_from(r: &Value) -> Option<CallRequest> {
    let platform = r.get("platform")?;
    Some(CallRequest::RunStep {
        step: text(r, "step")?,
        arguments: map_from(r.get("arguments")?)?,
        workload: Uuid::parse_str(r.get("workload")?.as_str()?).ok()?,
        platform: Platform { os: text(platform, "os")?, arch: text(platform, "arch")? },
    })
}

fn step_result_to(r: &StepResult) -> Value {
    let artifacts: Vec<Value> = r
        .artifacts
        .iter()
        .map(|a| {
            json!({
                "path": a.path, "url": a.url, "sha256": a.sha256, "size": a.size,
                "mode": a.mode, "extract": a.extract,
            })
        })
        .collect();
    json!({ "step_result": { "artifacts": artifacts, "pinned": r.pinned, "resolved": r.resolved } })
}

fn artifact_from(a: &Value) -> Option<StepArtifact> {
    Some(StepArtifact {
        path: text(a, "path")?,
        url: text(a, "url")?,
        sha256: text(a, "sha256")?,
        size: a.get("size")?.as_u64()?,
        mode: u32::try_from(a.get("mode")?.as_u64()?).ok()?,
        extract: a.get("extract")?.as_bool()?,
    })
}

fn step_result_from(r: &Value) -> Option<CallOutcome> {
    let artifacts = r.get("artifacts")?.as_array()?;
    Some(CallOutcome::StepResult(StepResult {
        artifacts: artifacts.iter().map(artifact_from).collect::<Option<_>>()?,
        pinned: r.get("pinned")?.as_bool()?,
        resolved: text(r, "resolved")?,
    }))
}

/// A request to store.
pub(crate) fn request_to(r: &CallRequest) -> Value {
    match r {
        CallRequest::Ping { nonce } => json!({ "ping": { "nonce": nonce } }),
        CallRequest::RunStep { step, arguments, workload, platform } => json!({
            "run_step": {
                "step": step,
                "arguments": map_to(arguments),
                "workload": workload.to_string(),
                "platform": { "os": platform.os, "arch": platform.arch },
            }
        }),
        CallRequest::GrantAccess { .. }
        | CallRequest::ListObjects { .. }
        | CallRequest::DeleteObjects { .. } => {
            // Always some for these three.
            super::backup::request_to(r).unwrap_or(Value::Null)
        }
        CallRequest::GenerateKey { .. } | CallRequest::UnwrapKey { .. } => {
            // Always some for these two.
            super::keys::request_to(r).unwrap_or(Value::Null)
        }
        CallRequest::VerifyMembers(_) | CallRequest::Assert(_) | CallRequest::Authorize(_) => {
            // Always some for these three.
            super::identity::request_to(r).unwrap_or(Value::Null)
        }
        CallRequest::ApplyRecords { owner, records } => json!({
            "apply_records": {
                "owner": owner,
                "records": records.iter().map(record_to).collect::<Vec<_>>(),
            }
        }),
    }
}

/// A stored request.
pub(crate) fn request_from(v: &Value) -> StoreResult<CallRequest> {
    if let Some(ping) = v.get("ping") {
        let nonce = text(ping, "nonce").ok_or_else(|| corrupt("request", v))?;
        return Ok(CallRequest::Ping { nonce });
    }
    if let Some(r) = super::backup::request_from(v) {
        return Ok(r);
    }
    if let Some(r) = super::keys::request_from(v) {
        return Ok(r);
    }
    if let Some(r) = super::identity::request_from(v) {
        return Ok(r);
    }
    if let Some(r) = v.get("run_step") {
        return run_step_from(r).ok_or_else(|| corrupt("request", v));
    }
    let apply = v.get("apply_records").ok_or_else(|| corrupt("request", v))?;
    let owner = text(apply, "owner").ok_or_else(|| corrupt("request", v))?;
    let records = apply
        .get("records")
        .and_then(Value::as_array)
        .and_then(|rs| rs.iter().map(record_from).collect::<Option<Vec<_>>>())
        .ok_or_else(|| corrupt("request", v))?;
    Ok(CallRequest::ApplyRecords { owner, records })
}

/// An outcome to store.
pub(crate) fn outcome_to(o: &CallOutcome) -> Value {
    match o {
        CallOutcome::Pong { nonce } => json!({ "pong": { "nonce": nonce } }),
        CallOutcome::StepResult(r) => step_result_to(r),
        CallOutcome::Access(_) | CallOutcome::Objects { .. } | CallOutcome::Deleted { .. } => {
            // Always some for these three.
            super::backup::outcome_to(o).unwrap_or(Value::Null)
        }
        CallOutcome::KeyMaterial { .. } => super::keys::outcome_to(o).unwrap_or(Value::Null),
        CallOutcome::Members(_) | CallOutcome::Asserted(_) | CallOutcome::Authorization(_) => {
            // Always some for these three.
            super::identity::outcome_to(o).unwrap_or(Value::Null)
        }
        CallOutcome::RecordsApplied { created, updated, deleted } => json!({
            "records_applied": { "created": created, "updated": updated, "deleted": deleted }
        }),
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
    if let Some(o) = super::backup::outcome_from(v) {
        return Ok(o);
    }
    if let Some(o) = super::keys::outcome_from(v) {
        return Ok(o);
    }
    if let Some(o) = super::identity::outcome_from(v) {
        return Ok(o);
    }
    if let Some(r) = v.get("step_result") {
        return step_result_from(r).ok_or_else(|| corrupt("outcome", v));
    }
    if let Some(a) = v.get("records_applied") {
        let n = |k: &str| a.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
        let (Some(created), Some(updated), Some(deleted)) =
            (n("created"), n("updated"), n("deleted"))
        else {
            return Err(corrupt("outcome", v));
        };
        return Ok(CallOutcome::RecordsApplied { created, updated, deleted });
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
        let record = DnsRecord {
            name: "play.example.com".into(),
            kind: RecordType::Srv,
            ttl: 300,
            values: vec!["0 5 25565 n1.example.com".into()],
        };
        let r = CallRequest::ApplyRecords { owner: "ep_1".into(), records: vec![record] };
        assert_eq!(request_from(&request_to(&r)).unwrap(), r);
        for o in [
            CallOutcome::Pong { nonce: "n1".into() },
            CallOutcome::RecordsApplied { created: 1, updated: 2, deleted: 3 },
            CallOutcome::StepResult(StepResult {
                artifacts: vec![StepArtifact {
                    path: "game".into(),
                    url: "https://cdn.example/g.tar".into(),
                    sha256: "a".repeat(64),
                    size: 5,
                    mode: 0o644,
                    extract: true,
                }],
                pinned: false,
                resolved: "manifest 1".into(),
            }),
            CallOutcome::Failed(CallFailure::deadline()),
            CallOutcome::Failed(CallFailure::unknown()),
        ] {
            assert_eq!(outcome_from(&outcome_to(&o)).unwrap(), o);
        }
        assert!(outcome_from(&json!({ "other": {} })).is_err());
    }
}
