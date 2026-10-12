// SPDX-License-Identifier: AGPL-3.0-or-later

//! A dynamically typed value: recipe configuration, step detail, driver configuration. Typed by
//! the schema that governs it, never by convention (`korpis.v1.Value`).

use std::collections::BTreeMap;

/// A dynamically typed value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Present and explicitly null.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// A floating-point number.
    Double(f64),
    /// A string.
    String(String),
    /// Bytes.
    Bytes(Vec<u8>),
    /// A list.
    List(Vec<Value>),
    /// A map with string keys, ordered.
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// The value as a number, if it is one.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(int_to_f64(*i)),
            Self::Double(d) => Some(*d),
            _ => None,
        }
    }

    /// The value as a string, if it is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The name of the value's type, for messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Int(_) => "integer",
            Self::Double(_) => "number",
            Self::String(_) => "string",
            Self::Bytes(_) => "bytes",
            Self::List(_) => "list",
            Self::Map(_) => "map",
        }
    }
}

/// An integer as a float, exactly up to 2^53 in magnitude.
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "the one place configuration integers become floats for comparison and arithmetic"
)]
pub fn int_to_f64(i: i64) -> f64 {
    i as f64
}

/// An unsigned amount as a float, exactly up to 2^53: capacities and counts become ratios.
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "the one place amounts become floats for the scheduler's ratios"
)]
pub fn uint_to_f64(n: u64) -> f64 {
    n as f64
}

/// A finite float rounded down to an integer, if it fits.
#[must_use]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "range-checked before the cast"
)]
pub fn floor_to_i64(x: f64) -> Option<i64> {
    let whole = x.floor();
    (whole.is_finite() && (-9.2e18..=9.2e18).contains(&whole)).then_some(whole as i64)
}

/// A configuration map.
pub type ValueMap = BTreeMap<String, Value>;
