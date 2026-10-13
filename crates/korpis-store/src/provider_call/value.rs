// SPDX-License-Identifier: AGPL-3.0-or-later

//! A typed value in a stored call: one key naming its type, so an integer stays an integer, a
//! double a double, and bytes bytes, which plain JSON would conflate.

use korpis_core::value::{Value, ValueMap};
use serde_json::{Value as Json, json};

/// A value to store.
pub(crate) fn value_to(v: &Value) -> Json {
    match v {
        Value::Null => json!({ "null": true }),
        Value::Bool(b) => json!({ "bool": b }),
        Value::Int(i) => json!({ "int": i }),
        Value::Double(d) => json!({ "double": d }),
        Value::String(s) => json!({ "string": s }),
        Value::Bytes(b) => {
            let hex = b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
                use std::fmt::Write as _;
                let _ = write!(s, "{x:02x}");
                s
            });
            json!({ "bytes": hex })
        }
        Value::List(l) => json!({ "list": l.iter().map(value_to).collect::<Vec<_>>() }),
        Value::Map(m) => json!({ "map": map_to(m) }),
    }
}

/// A map of values to store.
pub(crate) fn map_to(m: &ValueMap) -> Json {
    Json::Object(m.iter().map(|(k, v)| (k.clone(), value_to(v))).collect())
}

fn bytes(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| hex.get(i..i + 2).and_then(|p| u8::from_str_radix(p, 16).ok()))
        .collect()
}

/// A stored value.
pub(crate) fn value_from(j: &Json) -> Option<Value> {
    let (tag, v) = j.as_object()?.iter().next()?;
    Some(match tag.as_str() {
        "null" => Value::Null,
        "bool" => Value::Bool(v.as_bool()?),
        "int" => Value::Int(v.as_i64()?),
        "double" => Value::Double(v.as_f64()?),
        "string" => Value::String(v.as_str()?.to_owned()),
        "bytes" => Value::Bytes(bytes(v.as_str()?)?),
        "list" => Value::List(v.as_array()?.iter().map(value_from).collect::<Option<_>>()?),
        "map" => Value::Map(map_from(v)?),
        _ => return None,
    })
}

/// A stored map of values.
pub(crate) fn map_from(j: &Json) -> Option<ValueMap> {
    j.as_object()?.iter().map(|(k, v)| Some((k.clone(), value_from(v)?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_value_keeps_its_type() {
        let mut m = ValueMap::new();
        m.insert("n".into(), Value::Null);
        m.insert("b".into(), Value::Bool(true));
        m.insert("i".into(), Value::Int(730));
        m.insert("d".into(), Value::Double(1.0));
        m.insert("s".into(), Value::String("public".into()));
        m.insert("x".into(), Value::Bytes(vec![0, 255]));
        m.insert("l".into(), Value::List(vec![Value::Int(1)]));
        m.insert("m".into(), Value::Map(m.clone()));
        assert_eq!(map_from(&map_to(&m)), Some(m));
    }
}
