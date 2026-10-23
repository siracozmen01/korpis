// SPDX-License-Identifier: AGPL-3.0-or-later

//! Health providers' calls in the stored queue (§5 of docs/04-runtimes.md): a query probe's
//! step, with the rounds a node ran, each sent and read byte as hex, and the provider's answer,
//! the next round or the verdict. A query's bytes are a game's status, never a secret, and they
//! are kept an hour with every finished call.

use super::keys::{hex, unhex};
use korpis_core::extension::health::{HealthNext, HealthStep, Round, Transport};
use korpis_core::extension::{CallOutcome, CallRequest};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn round_to(r: &Round) -> Value {
    json!({ "sent": hex(&r.sent), "received": hex(&r.received), "closed": r.closed })
}

fn round_from(v: &Value) -> Option<Round> {
    Some(Round {
        sent: unhex(v, "sent")?,
        received: unhex(v, "received")?,
        closed: v.get("closed")?.as_bool()?,
    })
}

/// A health request to store, if `r` is one.
pub(crate) fn request_to(r: &CallRequest) -> Option<Value> {
    let CallRequest::HealthStep(s) = r else { return None };
    Some(json!({ "health_step": {
        "protocol": s.protocol,
        "workload": s.workload.to_string(),
        "endpoint": s.endpoint,
        "transport": s.transport.as_str(),
        "rounds": s.rounds.iter().map(round_to).collect::<Vec<_>>(),
    } }))
}

/// A stored health request, if `v` is one.
pub(crate) fn request_from(v: &Value) -> Option<CallRequest> {
    let s = v.get("health_step")?;
    let transport = match s.get("transport")?.as_str()? {
        "udp" => Transport::Udp,
        "tcp" => Transport::Tcp,
        _ => return None,
    };
    let rounds = s.get("rounds")?.as_array()?;
    Some(CallRequest::HealthStep(HealthStep {
        protocol: text(s, "protocol")?,
        workload: Uuid::parse_str(s.get("workload")?.as_str()?).ok()?,
        endpoint: text(s, "endpoint")?,
        transport,
        rounds: rounds.iter().map(round_from).collect::<Option<_>>()?,
    }))
}

/// A health outcome to store, if `o` is one.
pub(crate) fn outcome_to(o: &CallOutcome) -> Option<Value> {
    let CallOutcome::HealthNext(n) = o else { return None };
    Some(match n {
        HealthNext::Send { data, read_at_most, wait } => json!({ "health_send": {
            "data": hex(data),
            "read_at_most": read_at_most,
            "wait_ms": u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        } }),
        HealthNext::Verdict { healthy, reason, detail } => json!({ "health_verdict": {
            "healthy": healthy, "reason": reason, "detail": detail,
        } }),
    })
}

/// A stored health outcome, if `v` is one.
pub(crate) fn outcome_from(v: &Value) -> Option<CallOutcome> {
    if let Some(s) = v.get("health_send") {
        return Some(CallOutcome::HealthNext(HealthNext::Send {
            data: unhex(s, "data")?,
            read_at_most: u32::try_from(s.get("read_at_most")?.as_u64()?).ok()?,
            wait: Duration::from_millis(s.get("wait_ms")?.as_u64()?),
        }));
    }
    let d = v.get("health_verdict")?;
    let detail = d.get("detail")?.as_object()?;
    Some(CallOutcome::HealthNext(HealthNext::Verdict {
        healthy: d.get("healthy")?.as_bool()?,
        reason: text(d, "reason")?,
        detail: detail
            .iter()
            .map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect::<Option<_>>()?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_and_both_answers_survive_the_round_trip() {
        let step = CallRequest::HealthStep(HealthStep {
            protocol: "korpis-test/echo".into(),
            workload: Uuid::now_v7(),
            endpoint: "game".into(),
            transport: Transport::Udp,
            rounds: vec![Round { sent: b"ping".to_vec(), received: vec![0, 255], closed: false }],
        });
        assert_eq!(request_to(&step).and_then(|v| request_from(&v)), Some(step));
        let send = CallOutcome::HealthNext(HealthNext::Send {
            data: vec![],
            read_at_most: 512,
            wait: Duration::from_millis(1500),
        });
        assert_eq!(outcome_to(&send).and_then(|v| outcome_from(&v)), Some(send));
        let verdict = CallOutcome::HealthNext(HealthNext::Verdict {
            healthy: true,
            reason: "answered".into(),
            detail: [("map".to_owned(), "de_dust2".to_owned())].into(),
        });
        assert_eq!(outcome_to(&verdict).and_then(|v| outcome_from(&v)), Some(verdict));
    }
}
