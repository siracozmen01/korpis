// SPDX-License-Identifier: AGPL-3.0-or-later

//! Names, quantities, durations, and values as the document writes them.

use korpis_core::quantity::{Quantity, parse_duration};
use korpis_core::recipe::RecipeError;
use korpis_core::value::Value;
use korpis_core::workload::spec::endpoints::{Exposure, Protocol};
use korpis_core::workload::spec::{ConsoleMode, IsolationTier, Lifecycle};
use std::time::Duration;

/// Looks `name` up in a table of the document's spellings.
pub fn named<T: Copy>(field: &str, name: &str, table: &[(&str, T)]) -> Result<T, RecipeError> {
    table.iter().find(|(n, _)| *n == name).map(|(_, v)| *v).ok_or_else(|| {
        let names: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
        RecipeError::new(field, format!("{name:?} is not one of {names:?}"))
    })
}

pub fn tier(field: &str, s: &str) -> Result<IsolationTier, RecipeError> {
    use IsolationTier as T;
    named(
        field,
        s,
        &[
            ("process", T::Process),
            ("container", T::Container),
            ("microvm", T::MicroVm),
            ("vm", T::Vm),
        ],
    )
}

pub fn protocol(field: &str, s: &str) -> Result<Protocol, RecipeError> {
    use Protocol as P;
    named(
        field,
        s,
        &[
            ("tcp", P::Tcp),
            ("udp", P::Udp),
            ("http", P::Http),
            ("https", P::Https),
            ("quic", P::Quic),
        ],
    )
}

pub fn exposure(field: &str, s: &str) -> Result<Exposure, RecipeError> {
    use Exposure as E;
    named(
        field,
        s,
        &[
            ("node_port", E::NodePort),
            ("node_bound", E::NodeBound),
            ("stable", E::Stable),
            ("ingress", E::Ingress),
            ("dedicated_ip", E::DedicatedIp),
            ("overlay_only", E::OverlayOnly),
            ("none", E::None),
        ],
    )
}

pub fn lifecycle(field: &str, s: &str) -> Result<Lifecycle, RecipeError> {
    use Lifecycle as L;
    named(
        field,
        s,
        &[("persistent", L::Persistent), ("task", L::Task), ("scheduled", L::Scheduled)],
    )
}

pub fn console(field: &str, s: &str) -> Result<ConsoleMode, RecipeError> {
    use ConsoleMode as C;
    named(
        field,
        s,
        &[
            ("none", C::None),
            ("logs", C::Logs),
            ("interactive", C::Interactive),
            ("serial", C::Serial),
        ],
    )
}

pub fn quantity(field: &str, s: &str) -> Result<Quantity, RecipeError> {
    Quantity::parse(s).map_err(|e| RecipeError::new(field, e.to_string()))
}

/// A byte size such as `10Gi`.
pub fn bytes(field: &str, s: Option<&str>) -> Result<u64, RecipeError> {
    let Some(s) = s else { return Ok(0) };
    u64::try_from(quantity(field, s)?.units()).map_err(|_| RecipeError::new(field, "too large"))
}

/// CPU such as `500m` or `2`, in millicores.
pub fn millicores(field: &str, s: Option<&str>) -> Result<u64, RecipeError> {
    let Some(s) = s else { return Ok(0) };
    u64::try_from(quantity(field, s)?.millis()).map_err(|_| RecipeError::new(field, "too large"))
}

pub fn duration(field: &str, s: &str) -> Result<Duration, RecipeError> {
    parse_duration(s).map_err(|e| RecipeError::new(field, e.to_string()))
}

/// A document value as a domain value. Integers stay integers; other numbers become floats.
pub fn value(field: &str, v: &serde_json::Value) -> Result<Value, RecipeError> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Double(
                n.as_f64().ok_or_else(|| RecipeError::new(field, "number out of range"))?,
            ),
        },
        J::String(s) => Value::String(s.clone()),
        J::Array(a) => Value::List(a.iter().map(|x| value(field, x)).collect::<Result<_, _>>()?),
        J::Object(m) => Value::Map(
            m.iter()
                .map(|(k, x)| Ok((k.clone(), value(field, x)?)))
                .collect::<Result<_, RecipeError>>()?,
        ),
    })
}
