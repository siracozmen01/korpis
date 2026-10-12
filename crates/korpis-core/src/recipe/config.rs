// SPDX-License-Identifier: AGPL-3.0-or-later

//! Typed, permissioned configuration (§5 of docs/09-recipes.md, Rule K-17): the schema a recipe
//! declares, and the rules it must satisfy when the recipe is loaded.

use super::derive::Expr;
use crate::value::Value;
use regex::Regex;
use std::collections::BTreeSet;

/// A field's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigType {
    /// Text.
    String,
    /// A whole number.
    Integer,
    /// A number.
    Number,
    /// True or false.
    Boolean,
    /// A resource quantity such as `4Gi` or `500m`.
    Quantity,
    /// One of `choices`.
    Enum,
    /// A duration such as `1h30m`.
    Duration,
}

impl ConfigType {
    fn numeric(self) -> bool {
        matches!(self, Self::Integer | Self::Number | Self::Quantity)
    }
}

/// Who may change a field, as a rule the gateway evaluates (§5 of docs/09-recipes.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FieldPermission {
    /// A holder of `workload.write` on the workload.
    Tenant,
    /// A holder of `workload.write` through a grant scoped to the parent of the owning
    /// organization or above; for a root organization's own workloads, its own scope.
    Operator,
    /// Korpis only: derived or fixed.
    System,
}

/// How a value reaches the software (§5.2 of docs/09-recipes.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigDelivery {
    /// Rendered by templates into files in the workload's volumes, which the tenant can write.
    /// §5.2 of docs/09-recipes.md calls this channel `file`.
    Template,
    /// An environment variable set by the runtime.
    Env,
    /// A command-line argument set by the runtime; visible in the process table.
    Arg,
    /// A file Korpis renders into a read-only mount outside every tenant-writable path.
    ManagedFile,
}

/// One field.
#[derive(Debug, Clone)]
pub struct ConfigField {
    /// The key, `[a-z][a-z0-9_]*`.
    pub key: String,
    /// Its type.
    pub ty: ConfigType,
    /// For the generated form.
    pub label: String,
    /// Who may change it.
    pub permission: FieldPermission,
    /// The value when none is declared.
    pub default: Option<Value>,
    /// A fixed value nobody can change.
    pub constant: Option<Value>,
    /// Lower bound, for numeric types.
    pub min: Option<f64>,
    /// Upper bound, for numeric types.
    pub max: Option<f64>,
    /// Longest value, in characters, for strings.
    pub max_length: Option<u32>,
    /// The pattern the whole value must match, for strings.
    pub pattern: Option<Regex>,
    /// The allowed values, if restricted; required for enums.
    pub choices: Vec<Value>,
    /// Redacted from streams, plans and effects; read back only with `workload.secret.read`.
    pub secret: bool,
    /// Not shown in the generated form.
    pub hidden: bool,
    /// Computed by the control plane at declaration; makes the field system-set.
    pub derived: Option<Expr>,
    /// How it reaches the software.
    pub deliver: ConfigDelivery,
    /// For env: the variable name; for arg: the argument template.
    pub deliver_as: String,
}

/// A recipe's configuration schema, validated.
#[derive(Debug, Clone, Default)]
pub struct ConfigSchema {
    fields: Vec<ConfigField>,
}

/// Why a schema is refused when the recipe is loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("config field {key:?}: {message}")]
pub struct SchemaError {
    /// The field.
    pub key: String,
    /// What is wrong.
    pub message: String,
}

fn err(key: &str, message: impl Into<String>) -> SchemaError {
    SchemaError { key: key.to_owned(), message: message.into() }
}

/// Compiles a pattern so that it must match the whole value.
pub fn anchored(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{pattern})$"))
}

fn valid_key(k: &str) -> bool {
    let mut b = k.bytes();
    k.len() <= 64
        && b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

fn valid_env_name(n: &str) -> bool {
    let mut b = n.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn check_shape(f: &ConfigField) -> Result<(), SchemaError> {
    let k = &f.key;
    if !valid_key(k) {
        return Err(err(k, "keys are [a-z][a-z0-9_]*, at most 64 bytes"));
    }
    if (f.min.is_some() || f.max.is_some()) && !f.ty.numeric() {
        return Err(err(k, "min and max apply to numeric types only"));
    }
    if (f.max_length.is_some() || f.pattern.is_some()) && f.ty != ConfigType::String {
        return Err(err(k, "max_length and pattern apply to strings only"));
    }
    if f.ty == ConfigType::Enum && f.choices.is_empty() {
        return Err(err(k, "an enum needs choices"));
    }
    Ok(())
}

fn check_authority(f: &ConfigField) -> Result<(), SchemaError> {
    let k = &f.key;
    let system_set = f.derived.is_some() || f.constant.is_some();
    if f.derived.is_some() && f.permission != FieldPermission::System {
        return Err(err(k, "a derived field is system-set: declare permission system"));
    }
    if f.permission == FieldPermission::System && !system_set {
        return Err(err(k, "a system field must be derived or constant"));
    }
    if f.derived.is_some() && !f.ty.numeric() {
        return Err(err(k, "a derived field must be integer, number, or quantity"));
    }
    // §5.2: a protected value may only travel by a channel the tenant cannot write.
    let protected = f.permission == FieldPermission::Operator || f.secret;
    if protected && f.deliver == ConfigDelivery::Template {
        return Err(err(k, "operator and secret fields cannot be delivered through tenant files"));
    }
    if f.secret && f.deliver == ConfigDelivery::Arg {
        return Err(err(k, "a secret cannot be an argument: arguments are in the process table"));
    }
    match f.deliver {
        ConfigDelivery::Env if !valid_env_name(&f.deliver_as) => {
            Err(err(k, "deliver env needs deliver_as naming the variable"))
        }
        ConfigDelivery::Arg if f.deliver_as.is_empty() => {
            Err(err(k, "deliver arg needs deliver_as giving the argument"))
        }
        _ => Ok(()),
    }
}

impl ConfigSchema {
    /// Validates a schema. Defaults, constants and choices are checked against their own field,
    /// and derived expressions may read only numeric fields that are not themselves derived.
    pub fn new(fields: Vec<ConfigField>) -> Result<Self, SchemaError> {
        let mut seen = BTreeSet::new();
        for f in &fields {
            if !seen.insert(f.key.as_str()) {
                return Err(err(&f.key, "declared twice"));
            }
            check_shape(f)?;
            check_authority(f)?;
            for v in f.default.iter().chain(&f.constant).chain(&f.choices) {
                super::resolve::check_value(f, v).map_err(|e| err(&f.key, e.message))?;
            }
        }
        let schema = Self { fields };
        schema.check_references()?;
        Ok(schema)
    }

    fn check_references(&self) -> Result<(), SchemaError> {
        for f in &self.fields {
            let Some(expr) = &f.derived else { continue };
            for key in expr.config_keys() {
                self.check_reference(&f.key, key)?;
            }
        }
        Ok(())
    }

    /// A derived field may read only numeric fields that are not themselves derived, so
    /// evaluation needs no ordering and cannot cycle.
    fn check_reference(&self, from: &str, key: &str) -> Result<(), SchemaError> {
        match self.field(key) {
            Some(r) if r.ty.numeric() && r.derived.is_none() => Ok(()),
            Some(_) => Err(err(from, format!("config.{key} is not a plain number"))),
            None => Err(err(from, format!("config.{key} is not declared"))),
        }
    }

    /// The fields, in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[ConfigField] {
        &self.fields
    }

    /// One field by key.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&ConfigField> {
        self.fields.iter().find(|f| f.key == key)
    }
}
