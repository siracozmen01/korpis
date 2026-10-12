// SPDX-License-Identifier: AGPL-3.0-or-later

//! Validating declared configuration against a schema (Rule K-17). Invalid intents never exist:
//! configuration is checked at declaration, defaults, constants and derived values are written
//! into the intent, and every field whose value changes is reported with its permission so the
//! application can check the caller's authority for it.

use super::config::{ConfigField, ConfigSchema, ConfigType, FieldPermission};
use super::derive::{Inputs, Part, Resource};
use crate::quantity::{Quantity, parse_duration};
use crate::value::{Value, ValueMap, floor_to_i64};

/// Why declared configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("config.{key}: {message}")]
pub struct ConfigError {
    /// The field.
    pub key: String,
    /// What is wrong.
    pub message: String,
}

fn err(key: &str, message: impl Into<String>) -> ConfigError {
    ConfigError { key: key.to_owned(), message: message.into() }
}

/// A field whose value differs from what it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The field.
    pub key: String,
    /// Who may make this change.
    pub permission: FieldPermission,
    /// Whether effects may record only that it changed, never the value.
    pub secret: bool,
}

/// Configuration as it will be written into the intent.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// Every field with a value: declared, default, constant, or derived.
    pub values: ValueMap,
    /// The fields whose value differs from the previous intent's, or from the schema's own
    /// values for a new workload.
    pub changes: Vec<Change>,
}

fn numeric(f: &ConfigField, v: &Value) -> Result<f64, String> {
    match (f.ty, v) {
        (ConfigType::Integer, Value::Int(_))
        | (ConfigType::Number, Value::Int(_) | Value::Double(_)) => {
            v.as_f64().ok_or_else(|| "not a number".to_owned())
        }
        (ConfigType::Quantity, Value::Int(i)) if *i >= 0 => {
            v.as_f64().ok_or_else(|| format!("{i} is not a quantity"))
        }
        (ConfigType::Quantity, Value::String(s)) => {
            Quantity::parse(s).map(Quantity::as_f64).map_err(|e| e.to_string())
        }
        _ => Err(format!("expected {:?}, found {}", f.ty, v.type_name())),
    }
}

fn check_string(f: &ConfigField, s: &str) -> Result<(), String> {
    if f.max_length.is_some_and(|m| s.chars().count() > usize::try_from(m).unwrap_or(usize::MAX)) {
        return Err("longer than max_length".into());
    }
    if f.pattern.as_ref().is_some_and(|p| !p.is_match(s)) {
        return Err("does not match the pattern".into());
    }
    Ok(())
}

/// Whether `v` is a valid value for `f`, ignoring who sets it.
pub(crate) fn check_value(f: &ConfigField, v: &Value) -> Result<(), ConfigError> {
    let fail = |m: String| err(&f.key, m);
    match (f.ty, v) {
        (ConfigType::String, Value::String(s)) => check_string(f, s).map_err(fail)?,
        (ConfigType::Boolean, Value::Bool(_)) | (ConfigType::Enum, _) => {}
        (ConfigType::Duration, Value::String(s)) => {
            parse_duration(s).map_err(|e| fail(e.to_string()))?;
        }
        (ConfigType::Integer | ConfigType::Number | ConfigType::Quantity, _) => {
            let n = numeric(f, v).map_err(fail)?;
            if f.min.is_some_and(|m| n < m) || f.max.is_some_and(|m| n > m) {
                return Err(fail(format!("{n} is outside [{:?}, {:?}]", f.min, f.max)));
            }
        }
        _ => return Err(fail(format!("expected {:?}, found {}", f.ty, v.type_name()))),
    }
    if !f.choices.is_empty() && !f.choices.contains(v) {
        return Err(fail("not one of the choices".into()));
    }
    Ok(())
}

struct Context<'a> {
    schema: &'a ConfigSchema,
    values: &'a ValueMap,
    resources: &'a dyn Fn(Resource, Part) -> f64,
}

impl Inputs for Context<'_> {
    fn resource(&self, resource: Resource, part: Part) -> f64 {
        (self.resources)(resource, part)
    }

    fn config(&self, key: &str) -> Option<f64> {
        numeric(self.schema.field(key)?, self.values.get(key)?).ok()
    }
}

fn derived_value(f: &ConfigField, n: f64) -> Result<Value, ConfigError> {
    Ok(match f.ty {
        ConfigType::Number => Value::Double(n),
        ConfigType::Quantity if n < 0.0 => return Err(err(&f.key, "derived quantity is negative")),
        _ => Value::Int(floor_to_i64(n).ok_or_else(|| err(&f.key, "derived value out of range"))?),
    })
}

impl ConfigSchema {
    /// Resolves declared configuration. `previous` is the previous intent's resolved
    /// configuration, absent for a new workload; `resources` reads the intent being declared.
    pub fn resolve(
        &self,
        declared: &ValueMap,
        previous: Option<&ValueMap>,
        resources: &dyn Fn(Resource, Part) -> f64,
    ) -> Result<Resolved, ConfigError> {
        if let Some(unknown) = declared.keys().find(|k| self.field(k).is_none()) {
            return Err(err(unknown, "not declared by the recipe"));
        }
        let mut values = ValueMap::new();
        for f in self.fields().iter().filter(|f| f.derived.is_none()) {
            let declared = declared.get(&f.key);
            if let (Some(c), Some(d)) = (&f.constant, declared)
                && c != d
            {
                return Err(err(&f.key, "fixed by the recipe"));
            }
            if let Some(v) = f.constant.as_ref().or(declared).or(f.default.as_ref()) {
                check_value(f, v)?;
                values.insert(f.key.clone(), v.clone());
            }
        }
        for f in self.fields() {
            let Some(expr) = &f.derived else { continue };
            if declared.contains_key(&f.key) {
                return Err(err(&f.key, "derived by Korpis, not declared"));
            }
            let ctx = Context { schema: self, values: &values, resources };
            let n = expr.eval(&ctx).map_err(|e| err(&f.key, e.to_string()))?;
            let v = derived_value(f, n)?;
            check_value(f, &v)?;
            values.insert(f.key.clone(), v);
        }
        let baseline = match previous {
            Some(p) => p.clone(),
            None => self.resolve_defaults(resources)?,
        };
        let changes = self
            .fields()
            .iter()
            .filter(|f| values.get(&f.key) != baseline.get(&f.key))
            .map(|f| Change { key: f.key.clone(), permission: f.permission, secret: f.secret })
            .collect();
        Ok(Resolved { values, changes })
    }

    /// The configuration a new workload has when nothing is declared, which is what declared
    /// values are compared with to find what a caller is changing.
    fn resolve_defaults(
        &self,
        resources: &dyn Fn(Resource, Part) -> f64,
    ) -> Result<ValueMap, ConfigError> {
        Ok(self.resolve(&ValueMap::new(), Some(&ValueMap::new()), resources)?.values)
    }
}
