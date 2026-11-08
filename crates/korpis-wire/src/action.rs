// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extension actions' wire forms: an invocation's arguments as a client sends them and an
//! extension receives them, and what the action answers.

use crate::{IdKind, Result, id, invalid, wire_id};
use korpis_core::extension::action::{ActionResult, ArgumentValue, Arguments, Invoke};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::action_value::Value as V;

/// The wire form of one argument's value.
#[must_use]
pub fn value_to_pb(v: &ArgumentValue) -> pb::ActionValue {
    let value = match v {
        ArgumentValue::Text(t) => V::Text(t.clone()),
        ArgumentValue::Int(n) => V::Int(*n),
        ArgumentValue::Flag(b) => V::Flag(*b),
        ArgumentValue::Workload(w) => V::Workload(wire_id(IdKind::Workload, *w)),
        ArgumentValue::Project(p) => V::Project(wire_id(IdKind::Project, *p)),
    };
    pb::ActionValue { value: Some(value), ..Default::default() }
}

/// One argument's value from the field `field`; a value of no type is refused.
pub fn value_from_pb(field: &str, v: &pb::ActionValue) -> Result<ArgumentValue> {
    Ok(match &v.value {
        Some(V::Text(t)) => ArgumentValue::Text(t.clone()),
        Some(V::Int(n)) => ArgumentValue::Int(*n),
        Some(V::Flag(b)) => ArgumentValue::Flag(*b),
        Some(V::Workload(w)) => ArgumentValue::Workload(id(IdKind::Workload, field, w)?),
        Some(V::Project(p)) => ArgumentValue::Project(id(IdKind::Project, field, p)?),
        None => return Err(invalid(field, "a value of one of the declared types")),
    })
}

/// The wire form of an invocation's arguments.
#[must_use]
pub fn arguments_to_pb(a: &Arguments) -> std::collections::BTreeMap<String, pb::ActionValue> {
    a.iter().map(|(k, v)| (k.clone(), value_to_pb(v))).collect()
}

/// An invocation's arguments from the map field `field`.
pub fn arguments_from_pb<'a>(
    field: &str,
    a: impl IntoIterator<Item = (&'a String, &'a pb::ActionValue)>,
) -> Result<Arguments> {
    a.into_iter()
        .map(|(k, v)| Ok((k.clone(), value_from_pb(&format!("{field}.{k}"), v)?)))
        .collect()
}

/// The wire form of what an extension is sent to perform an action.
#[must_use]
pub fn invoke_to_pb(i: &Invoke) -> pb::InvokeAction {
    pb::InvokeAction {
        action: i.action.clone(),
        arguments: arguments_to_pb(&i.arguments),
        invocation: i.invocation.clone(),
        invoker: i.invoker.clone(),
        ..Default::default()
    }
}

/// What an extension is sent, as it reads it.
pub fn invoke_from_pb(i: &pb::InvokeAction) -> Result<Invoke> {
    Ok(Invoke {
        action: i.action.clone(),
        arguments: arguments_from_pb("arguments", &i.arguments)?,
        invocation: i.invocation.clone(),
        invoker: i.invoker.clone(),
    })
}

/// The wire form of what an action answered.
#[must_use]
pub fn result_to_pb(r: &ActionResult) -> pb::ActionResult {
    pb::ActionResult { message: r.message.clone(), data: r.data.clone(), ..Default::default() }
}

/// What an action answered, as the control plane reads it.
#[must_use]
pub fn result_from_pb(r: &pb::ActionResult) -> ActionResult {
    ActionResult { message: r.message.clone(), data: r.data.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invocation_reads_back_as_it_was_sent() {
        let mut arguments = Arguments::new();
        arguments.insert("server".into(), ArgumentValue::Workload(uuid::Uuid::now_v7()));
        arguments.insert("why".into(), ArgumentValue::Text("griefing".into()));
        arguments.insert("minutes".into(), ArgumentValue::Int(10));
        let sent = Invoke {
            action: "kick".into(),
            arguments,
            invocation: "secret".into(),
            invoker: "user:1".into(),
        };
        assert_eq!(invoke_from_pb(&invoke_to_pb(&sent)).unwrap(), sent);
        let empty = pb::ActionValue::default();
        assert!(value_from_pb("arguments.x", &empty).is_err(), "a value of no type");
    }
}
