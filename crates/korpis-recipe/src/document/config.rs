// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `config` block (§5 of docs/09-recipes.md).

use super::dto;
use super::scalars::{named, value};
use korpis_core::recipe::config::anchored;
use korpis_core::recipe::derive::Expr;
use korpis_core::recipe::{
    ConfigDelivery, ConfigField, ConfigSchema, ConfigType, FieldPermission, RecipeError,
};

const TYPES: [(&str, ConfigType); 7] = [
    ("string", ConfigType::String),
    ("integer", ConfigType::Integer),
    ("number", ConfigType::Number),
    ("boolean", ConfigType::Boolean),
    ("quantity", ConfigType::Quantity),
    ("enum", ConfigType::Enum),
    ("duration", ConfigType::Duration),
];

const PERMISSIONS: [(&str, FieldPermission); 3] = [
    ("tenant", FieldPermission::Tenant),
    ("operator", FieldPermission::Operator),
    ("system", FieldPermission::System),
];

// §5.2 calls the tenant-file channel `file`; the wire calls it `template`. Both are accepted.
const CHANNELS: [(&str, ConfigDelivery); 5] = [
    ("template", ConfigDelivery::Template),
    ("file", ConfigDelivery::Template),
    ("env", ConfigDelivery::Env),
    ("arg", ConfigDelivery::Arg),
    ("managed_file", ConfigDelivery::ManagedFile),
];

fn field(i: usize, f: &dto::Field) -> Result<ConfigField, RecipeError> {
    let at = |n: &str| format!("config.fields[{i}].{n}");
    let ty = named(&at("type"), &f.ty, &TYPES)?;
    let default_permission =
        if f.derived.is_some() || f.constant.is_some() { "system" } else { "tenant" };
    let permission = named(
        &at("permission"),
        f.permission.as_deref().unwrap_or(default_permission),
        &PERMISSIONS,
    )?;
    let deliver = named(&at("deliver"), f.deliver.as_deref().unwrap_or("template"), &CHANNELS)?;
    let pattern = f
        .pattern
        .as_deref()
        .map(|p| anchored(p).map_err(|e| RecipeError::new(&at("pattern"), e.to_string())))
        .transpose()?;
    let derived = f
        .derived
        .as_deref()
        .map(|d| Expr::parse(d).map_err(|e| RecipeError::new(&at("derived"), e.to_string())))
        .transpose()?;
    let val = |v: &serde_json::Value| value(&at("value"), v);
    Ok(ConfigField {
        key: f.key.clone(),
        ty,
        label: f.label.clone(),
        permission,
        default: f.default.as_ref().map(val).transpose()?,
        constant: f.constant.as_ref().map(val).transpose()?,
        min: f.min,
        max: f.max,
        max_length: f.max_length,
        pattern,
        choices: f.choices.iter().map(val).collect::<Result<_, _>>()?,
        secret: f.secret,
        hidden: f.hidden,
        derived,
        deliver,
        deliver_as: f.deliver_as.clone(),
    })
}

/// The schema, validated by its own rules.
pub fn schema(c: Option<&dto::Config>) -> Result<ConfigSchema, RecipeError> {
    let Some(c) = c else { return Ok(ConfigSchema::default()) };
    let fields = c.fields.iter().enumerate().map(|(i, f)| field(i, f)).collect::<Result<_, _>>()?;
    ConfigSchema::new(fields).map_err(|e| RecipeError::new(&format!("config.{}", e.key), e.message))
}
