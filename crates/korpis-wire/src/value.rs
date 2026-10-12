// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dynamically typed values: `korpis.v1.Value`.

use crate::{Result, invalid};
use korpis_core::value::{Value, ValueMap};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind as V;

/// The wire form of a value.
#[must_use]
pub fn to_pb(v: &Value) -> pb::Value {
    let kind = match v {
        Value::Null => V::NullValue(pb::NullValue::NULL_VALUE_UNSPECIFIED.into()),
        Value::Bool(b) => V::BoolValue(*b),
        Value::Int(i) => V::IntValue(*i),
        Value::Double(d) => V::DoubleValue(*d),
        Value::String(s) => V::StringValue(s.clone()),
        Value::Bytes(b) => V::BytesValue(b.clone()),
        Value::List(l) => V::ListValue(Box::new(pb::ValueList {
            values: l.iter().map(to_pb).collect(),
            ..Default::default()
        })),
        Value::Map(m) => {
            V::MapValue(Box::new(pb::ValueMap { fields: map_to_pb(m), ..Default::default() }))
        }
    };
    pb::Value { kind: Some(kind), ..Default::default() }
}

/// The wire form of a value map.
#[must_use]
pub fn map_to_pb<M: FromIterator<(String, pb::Value)>>(m: &ValueMap) -> M {
    m.iter().map(|(k, v)| (k.clone(), to_pb(v))).collect()
}

/// A value from the field `field`. A value with no kind set is refused: absence is expressed by
/// leaving the key out, and an explicit null by `null_value`.
pub fn from_pb(field: &str, v: &pb::Value) -> Result<Value> {
    Ok(match v.kind.as_ref().ok_or_else(|| invalid(field, "a value must set one kind"))? {
        V::NullValue(_) => Value::Null,
        V::BoolValue(b) => Value::Bool(*b),
        V::IntValue(i) => Value::Int(*i),
        V::DoubleValue(d) => Value::Double(*d),
        V::StringValue(s) => Value::String(s.clone()),
        V::BytesValue(b) => Value::Bytes(b.clone()),
        V::ListValue(l) => Value::List(
            l.values
                .iter()
                .enumerate()
                .map(|(i, v)| from_pb(&format!("{field}[{i}]"), v))
                .collect::<Result<_>>()?,
        ),
        V::MapValue(m) => Value::Map(map_from_pb(field, &m.fields)?),
    })
}

/// A value map from the field `field`.
pub fn map_from_pb<'a>(
    field: &str,
    m: impl IntoIterator<Item = (&'a String, &'a pb::Value)>,
) -> Result<ValueMap> {
    m.into_iter().map(|(k, v)| Ok((k.clone(), from_pb(&format!("{field}.{k}"), v)?))).collect()
}
