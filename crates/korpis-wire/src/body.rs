// SPDX-License-Identifier: AGPL-3.0-or-later

//! The stored form of an intent body (§3.1 of docs/03-state.md): the `korpis.v1.IntentBody`
//! message in proto3 canonical JSON, **with defaults emitted**, beside the schema version it was
//! written under.
//!
//! The generated serializer omits every field that holds its default, as canonical JSON allows,
//! and a stored row that omits a field records whatever the default is when someone reads it
//! rather than what it was when the row was written (finding R10). So the encoder walks the
//! message with the compiled schema and writes every implicit-presence field that was left out:
//! zero, `false`, the empty string, an enum's zero value by name, an empty list or map. Fields
//! with explicit presence (members of a oneof, `optional` fields) and message fields that were
//! never set are left absent, because their absence is itself the value: "not set". Well-known
//! types (`google.protobuf.*`) have their own JSON forms and are left as they are.

use crate::spec::{body_from_pb, body_to_pb};
use crate::{WireError, invalid};
use buffa_descriptor::{DescriptorPool, FieldKind, MessageDescriptor, ScalarType, SingularKind};
use korpis_core::workload::IntentBody;
use korpis_proto::korpis::v1 as pb;
use serde_json::{Map, Value};
use std::sync::LazyLock;

/// The schema version recorded with every body this build writes. The compatibility rules of
/// §4 of docs/10-api.md hold within a package version, so every body written under `korpis.v1`
/// reads with any later `korpis.v1` schema; a `korpis.v2` would be a new reader beside this one.
pub const SCHEMA_VERSION: &str = "korpis.v1";

const BODY: &str = "korpis.v1.IntentBody";

static POOL: LazyLock<Result<DescriptorPool, String>> = LazyLock::new(|| {
    DescriptorPool::decode(korpis_proto::DESCRIPTOR_SET).map_err(|e| e.to_string())
});

fn pool() -> Result<&'static DescriptorPool, WireError> {
    POOL.as_ref().map_err(|e| invalid("schema", format!("the compiled schema does not load: {e}")))
}

/// Encodes a body for storage.
pub fn encode(body: &IntentBody) -> Result<String, WireError> {
    let pool = pool()?;
    let mut json = serde_json::to_value(body_to_pb(body))
        .map_err(|e| invalid("body", format!("does not encode: {e}")))?;
    let message = pool.message_by_name(BODY).ok_or_else(|| invalid("schema", "no IntentBody"))?;
    emit_defaults(pool, message, &mut json);
    serde_json::to_string(&json).map_err(|e| invalid("body", format!("does not encode: {e}")))
}

/// Decodes a stored body written under `schema_version`.
pub fn decode(json: &str, schema_version: &str) -> Result<IntentBody, WireError> {
    if schema_version != SCHEMA_VERSION {
        return Err(invalid(
            "schema_version",
            format!("{schema_version:?} is not a schema this build reads"),
        ));
    }
    let message: pb::IntentBody = serde_json::from_str(json)
        .map_err(|e| invalid("body", format!("does not parse as {BODY}: {e}")))?;
    body_from_pb("body", &message)
}

fn scalar_default(s: ScalarType) -> Value {
    match s {
        ScalarType::Bool => Value::Bool(false),
        ScalarType::String | ScalarType::Bytes => Value::String(String::new()),
        // Canonical JSON writes 64-bit integers as strings.
        ScalarType::Int64
        | ScalarType::Uint64
        | ScalarType::Fixed64
        | ScalarType::Sfixed64
        | ScalarType::Sint64 => Value::String("0".into()),
        ScalarType::Double
        | ScalarType::Float
        | ScalarType::Int32
        | ScalarType::Uint32
        | ScalarType::Fixed32
        | ScalarType::Sfixed32
        | ScalarType::Sint32 => Value::from(0),
    }
}

fn singular_default(pool: &DescriptorPool, k: SingularKind) -> Option<Value> {
    match k {
        SingularKind::Scalar(s) => Some(scalar_default(s)),
        SingularKind::Enum(e) => {
            pool.enumeration(e).value(0).map(|v| Value::String(v.name().to_owned()))
        }
        // An unset message is "not set", not a default.
        SingularKind::Message(_) => None,
    }
}

/// Fills the defaults of `json`, an encoding of `message`, and of every message inside it.
fn emit_defaults(pool: &DescriptorPool, message: &MessageDescriptor, json: &mut Value) {
    if message.full_name().starts_with("google.protobuf.") {
        return;
    }
    let Value::Object(obj) = json else { return };
    for field in message.fields() {
        let key = field.json_name();
        if field.oneof_index().is_some() {
            if let Some(v) = obj.get_mut(key) {
                descend(pool, field.kind(), v);
            }
            continue;
        }
        if !obj.contains_key(key) {
            let default = match field.kind() {
                FieldKind::Singular(k) => singular_default(pool, k),
                FieldKind::List(_) => Some(Value::Array(Vec::new())),
                FieldKind::Map { .. } => Some(Value::Object(Map::new())),
            };
            if let Some(d) = default {
                obj.insert(key.to_owned(), d);
            }
        }
        if let Some(v) = obj.get_mut(key) {
            descend(pool, field.kind(), v);
        }
    }
}

fn descend(pool: &DescriptorPool, kind: FieldKind, v: &mut Value) {
    match (kind, v) {
        (FieldKind::Singular(SingularKind::Message(m)), v @ Value::Object(_)) => {
            emit_defaults(pool, pool.message(m), v);
        }
        (FieldKind::List(SingularKind::Message(m)), Value::Array(items)) => {
            for item in items {
                emit_defaults(pool, pool.message(m), item);
            }
        }
        (FieldKind::Map { value: SingularKind::Message(m), .. }, Value::Object(entries)) => {
            for item in entries.values_mut() {
                emit_defaults(pool, pool.message(m), item);
            }
        }
        _ => {}
    }
}
