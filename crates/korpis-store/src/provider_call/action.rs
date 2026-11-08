// SPDX-License-Identifier: AGPL-3.0-or-later

//! Action calls in the stored queue: the action, its typed arguments, the invocation token and
//! the invoker; answered by a message and data. The token is stored with the call, which is why it
//! is worth nothing once its invocation has finished.

use korpis_core::extension::action::{ActionResult, ArgumentValue, Arguments, Invoke};
use korpis_core::extension::{CallOutcome, CallRequest};
use serde_json::{Map, Value, json};

fn value_to(v: &ArgumentValue) -> Value {
    match v {
        ArgumentValue::Text(t) => json!({ "text": t }),
        ArgumentValue::Int(n) => json!({ "int": n }),
        ArgumentValue::Flag(b) => json!({ "flag": b }),
        ArgumentValue::Workload(w) => json!({ "workload": w.to_string() }),
        ArgumentValue::Project(p) => json!({ "project": p.to_string() }),
    }
}

fn value_from(v: &Value) -> Option<ArgumentValue> {
    let uuid = |k: &str| v.get(k)?.as_str()?.parse().ok();
    if let Some(t) = v.get("text") {
        return Some(ArgumentValue::Text(t.as_str()?.to_owned()));
    }
    if let Some(n) = v.get("int") {
        return Some(ArgumentValue::Int(n.as_i64()?));
    }
    if let Some(b) = v.get("flag") {
        return Some(ArgumentValue::Flag(b.as_bool()?));
    }
    if v.get("workload").is_some() {
        return Some(ArgumentValue::Workload(uuid("workload")?));
    }
    Some(ArgumentValue::Project(uuid("project")?))
}

/// An action request to store, if `r` is one.
pub(crate) fn request_to(r: &CallRequest) -> Option<Value> {
    let CallRequest::InvokeAction(i) = r else { return None };
    let arguments: Map<String, Value> =
        i.arguments.iter().map(|(k, v)| (k.clone(), value_to(v))).collect();
    Some(json!({ "invoke_action": {
        "action": i.action, "arguments": arguments, "invocation": i.invocation,
        "invoker": i.invoker } }))
}

/// A stored action request, if `v` is one; `None` too where it is malformed.
pub(crate) fn request_from(v: &Value) -> Option<CallRequest> {
    let r = v.get("invoke_action")?;
    let text = |k: &str| Some(r.get(k)?.as_str()?.to_owned());
    let arguments = r
        .get("arguments")?
        .as_object()?
        .iter()
        .map(|(k, v)| Some((k.clone(), value_from(v)?)))
        .collect::<Option<Arguments>>()?;
    Some(CallRequest::InvokeAction(Invoke {
        action: text("action")?,
        arguments,
        invocation: text("invocation")?,
        invoker: text("invoker")?,
    }))
}

/// An action outcome to store, if `o` is one.
pub(crate) fn outcome_to(o: &CallOutcome) -> Option<Value> {
    let CallOutcome::ActionResult(r) = o else { return None };
    Some(json!({ "action_result": { "message": r.message, "data": r.data } }))
}

/// A stored action outcome, if `v` is one.
pub(crate) fn outcome_from(v: &Value) -> Option<CallOutcome> {
    let r = v.get("action_result")?;
    let text = |k: &str| Some(r.get(k)?.as_str()?.to_owned());
    Some(CallOutcome::ActionResult(ActionResult { message: text("message")?, data: text("data")? }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn an_invocation_is_stored_and_read_back_whole() {
        let mut arguments = Arguments::new();
        arguments.insert("a".into(), ArgumentValue::Text("x".into()));
        arguments.insert("b".into(), ArgumentValue::Int(-3));
        arguments.insert("c".into(), ArgumentValue::Flag(true));
        arguments.insert("d".into(), ArgumentValue::Workload(uuid::Uuid::from_u128(1)));
        arguments.insert("e".into(), ArgumentValue::Project(uuid::Uuid::from_u128(2)));
        let r = CallRequest::InvokeAction(Invoke {
            action: "kick".into(),
            arguments,
            invocation: "t".into(),
            invoker: "user:u".into(),
        });
        assert_eq!(request_from(&request_to(&r).unwrap()), Some(r));
        let o = CallOutcome::ActionResult(ActionResult { message: "m".into(), data: "{}".into() });
        assert_eq!(outcome_from(&outcome_to(&o).unwrap()), Some(o));
    }
}
