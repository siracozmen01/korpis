// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a resolved configuration reaches the software through the runtime (§5.2 of
//! docs/09-recipes.md): `env` fields become environment variables and `arg` fields command-line
//! arguments, in the schema's declaration order, each value written as text by its field's type.
//! Template fields are not the runtime's to deliver, and `managed_file` fields are named so the
//! caller can deliver them or say that it cannot.

use super::config::{ConfigDelivery, ConfigField, ConfigSchema, ConfigType};
use crate::quantity::{Quantity, parse_duration};
use crate::value::{Value, ValueMap};
use std::collections::BTreeMap;

/// Where an `arg` field's value goes in its `deliver_as` template.
pub const PLACEHOLDER: &str = "{}";

/// What the runtime sets for a configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delivered {
    /// Environment variables, by name.
    pub env: BTreeMap<String, String>,
    /// Command-line arguments, in declaration order.
    pub args: Vec<String>,
    /// The keys of `managed_file` fields that have a value: files the runtime does not render.
    pub managed: Vec<String>,
}

/// A value the runtime cannot write as text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("config field {key:?}: {message}")]
pub struct DeliveryError {
    /// The field.
    pub key: String,
    /// Why.
    pub message: String,
}

/// Seconds, exactly: `1h30m` is `5400`, `1500ms` is `1.5`.
fn seconds(d: std::time::Duration) -> String {
    let millis = d.subsec_millis();
    if millis == 0 {
        return d.as_secs().to_string();
    }
    let digits = format!("{millis:03}");
    format!("{}.{}", d.as_secs(), digits.trim_end_matches('0'))
}

/// `v` as the text the software reads. Quantities are written in base units and durations in
/// seconds, so the software sees one form whichever one was declared.
pub fn text(ty: ConfigType, v: &Value) -> Result<String, String> {
    Ok(match (ty, v) {
        (ConfigType::Quantity, Value::String(s)) => {
            Quantity::parse(s).map_err(|e| e.to_string())?.text()
        }
        (ConfigType::Duration, Value::String(s)) => {
            seconds(parse_duration(s).map_err(|e| e.to_string())?)
        }
        (_, Value::String(s)) => s.clone(),
        (_, Value::Int(i)) => i.to_string(),
        (_, Value::Double(d)) if d.is_finite() => d.to_string(),
        (_, Value::Bool(b)) => b.to_string(),
        (_, other) => return Err(format!("a {} is not written as text", other.type_name())),
    })
}

/// The arguments `f` contributes for the text `value`.
fn arguments(f: &ConfigField, value: &Value, rendered: &str) -> Vec<String> {
    let words = f.deliver_as.split(' ').filter(|w| !w.is_empty());
    if f.ty == ConfigType::Boolean && !f.deliver_as.contains(PLACEHOLDER) {
        // A flag: present when true, absent when false.
        return if value == &Value::Bool(true) {
            words.map(str::to_owned).collect()
        } else {
            vec![]
        };
    }
    words.map(|w| w.replace(PLACEHOLDER, rendered)).collect()
}

impl ConfigSchema {
    /// What the runtime sets for the resolved `values`. A field without a value, or with a null
    /// one, sets nothing.
    pub fn delivered(&self, values: &ValueMap) -> Result<Delivered, DeliveryError> {
        let mut out = Delivered::default();
        for f in self.fields() {
            let Some(v) = values.get(&f.key).filter(|v| **v != Value::Null) else { continue };
            let render =
                || text(f.ty, v).map_err(|message| DeliveryError { key: f.key.clone(), message });
            match f.deliver {
                ConfigDelivery::Template => {}
                ConfigDelivery::ManagedFile => out.managed.push(f.key.clone()),
                ConfigDelivery::Env => {
                    out.env.insert(f.deliver_as.clone(), render()?);
                }
                ConfigDelivery::Arg => out.args.extend(arguments(f, v, &render()?)),
            }
        }
        Ok(out)
    }
}
