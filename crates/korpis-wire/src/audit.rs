// SPDX-License-Identifier: AGPL-3.0-or-later

//! Effects.

use crate::{principal, timestamp, wire_id};

use korpis_core::audit::{Effect, Outcome};
use korpis_core::id::IdKind;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind as V;
use serde_json::{Map, Value};

/// A JSON value as a `korpis.v1.Value`.
fn value(v: &Value) -> pb::Value {
    let kind = match v {
        Value::Null => V::NullValue(pb::NullValue::NULL_VALUE_UNSPECIFIED.into()),
        Value::Bool(b) => V::BoolValue(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => V::IntValue(i),
            None => V::DoubleValue(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => V::StringValue(s.clone()),
        Value::Array(a) => V::ListValue(Box::new(pb::ValueList {
            values: a.iter().map(value).collect(),
            ..Default::default()
        })),
        Value::Object(m) => {
            V::MapValue(Box::new(pb::ValueMap { fields: map(Some(m)), ..Default::default() }))
        }
    };
    pb::Value { kind: Some(kind), ..Default::default() }
}

/// A JSON object as a map of `korpis.v1.Value`s.
pub(crate) fn map<M: FromIterator<(String, pb::Value)>>(m: Option<&Map<String, Value>>) -> M {
    m.into_iter().flatten().map(|(k, v)| (k.clone(), value(v))).collect()
}

/// The wire form of what went wrong: `reason` and `message` where the record names them, and
/// everything else as detail.
fn error_to_pb(m: &Map<String, Value>) -> pb::Error {
    let text = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    pb::Error {
        reason: text("reason"),
        message: text("message"),
        detail: m
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "reason" | "message"))
            .map(|(k, v)| (k.clone(), value(v)))
            .collect(),
        ..Default::default()
    }
}

fn outcome(o: Outcome) -> pb::EffectOutcome {
    match o {
        Outcome::Succeeded => pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED,
        Outcome::Failed => pb::EffectOutcome::EFFECT_OUTCOME_FAILED,
        Outcome::Skipped => pb::EffectOutcome::EFFECT_OUTCOME_SKIPPED,
        Outcome::RolledBack => pb::EffectOutcome::EFFECT_OUTCOME_ROLLED_BACK,
        Outcome::Denied => pb::EffectOutcome::EFFECT_OUTCOME_DENIED,
    }
}

/// The wire form of an effect.
#[must_use]
pub fn effect_to_pb(e: &Effect) -> pb::Effect {
    pb::Effect {
        id: wire_id(IdKind::Effect, e.id),
        plan: e.plan.map(|p| wire_id(IdKind::Plan, p)),
        step: e.step,
        workload: e.workload.map(|w| wire_id(IdKind::Workload, w)),
        node: e.node.map(|n| wire_id(IdKind::Node, n)),
        actor: principal::to_pb(&e.actor).into(),
        grant: e.grant.map(|g| wire_id(IdKind::Grant, g)),
        at: e.at.map(timestamp).into(),
        occurred_at: e.occurred_at.map(timestamp).into(),
        origin: e
            .origin
            .map(|o| pb::JournalOrigin {
                node: wire_id(IdKind::Node, o.node),
                seq: o.seq,
                ..Default::default()
            })
            .into(),
        action: e.action.clone(),
        outcome: outcome(e.outcome).into(),
        before: map(e.before.as_ref()),
        after: map(e.after.as_ref()),
        error: e.error.as_ref().map(error_to_pb).into(),
        count: e.count,
        window_end: e.window_end.map(timestamp).into(),
        ..Default::default()
    }
}
