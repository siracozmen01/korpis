// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a client is shown of a recipe (§5 of docs/09-recipes.md): `korpis.v1.RecipeMetadata` and
//! `korpis.v1.ConfigSchema`. Only ever sent: a client declares values against the schema, never
//! the schema itself.

use crate::value::to_pb as value_to_pb;
use korpis_core::recipe::{
    ConfigDelivery, ConfigField, ConfigSchema, ConfigType, FieldPermission, Metadata,
};
use korpis_proto::korpis::v1 as pb;

/// The wire form of a recipe's metadata.
#[must_use]
pub fn metadata_to_pb(m: &Metadata) -> pb::RecipeMetadata {
    pb::RecipeMetadata {
        name: m.name.clone(),
        version: m.version.clone(),
        description: m.description.clone(),
        maintainers: m.maintainers.clone(),
        license: m.license.clone(),
        annotations: m.annotations.clone().into_iter().collect(),
        locale: m.locale.clone(),
        ..Default::default()
    }
}

const fn type_to_pb(t: ConfigType) -> pb::ConfigType {
    match t {
        ConfigType::String => pb::ConfigType::CONFIG_TYPE_STRING,
        ConfigType::Integer => pb::ConfigType::CONFIG_TYPE_INTEGER,
        ConfigType::Number => pb::ConfigType::CONFIG_TYPE_NUMBER,
        ConfigType::Boolean => pb::ConfigType::CONFIG_TYPE_BOOLEAN,
        ConfigType::Quantity => pb::ConfigType::CONFIG_TYPE_QUANTITY,
        ConfigType::Enum => pb::ConfigType::CONFIG_TYPE_ENUM,
        ConfigType::Duration => pb::ConfigType::CONFIG_TYPE_DURATION,
    }
}

const fn permission_to_pb(p: FieldPermission) -> pb::FieldPermission {
    match p {
        FieldPermission::Tenant => pb::FieldPermission::FIELD_PERMISSION_TENANT,
        FieldPermission::Operator => pb::FieldPermission::FIELD_PERMISSION_OPERATOR,
        FieldPermission::System => pb::FieldPermission::FIELD_PERMISSION_SYSTEM,
    }
}

const fn delivery_to_pb(d: ConfigDelivery) -> pb::ConfigDelivery {
    match d {
        ConfigDelivery::Template => pb::ConfigDelivery::CONFIG_DELIVERY_TEMPLATE,
        ConfigDelivery::Env => pb::ConfigDelivery::CONFIG_DELIVERY_ENV,
        ConfigDelivery::Arg => pb::ConfigDelivery::CONFIG_DELIVERY_ARG,
        ConfigDelivery::ManagedFile => pb::ConfigDelivery::CONFIG_DELIVERY_MANAGED_FILE,
    }
}

/// A field's pattern as the recipe wrote it, without the anchoring it was compiled with.
fn pattern(f: &ConfigField) -> String {
    f.pattern.as_ref().map_or_else(String::new, |p| {
        let s = p.as_str();
        s.strip_prefix("^(?:").and_then(|s| s.strip_suffix(")$")).unwrap_or(s).to_owned()
    })
}

fn field_to_pb(f: &ConfigField) -> pb::ConfigField {
    pb::ConfigField {
        key: f.key.clone(),
        r#type: type_to_pb(f.ty).into(),
        label: f.label.clone(),
        permission: permission_to_pb(f.permission).into(),
        default: f.default.as_ref().map(value_to_pb).into(),
        r#const: f.constant.as_ref().map(value_to_pb).into(),
        min: f.min,
        max: f.max,
        max_length: f.max_length,
        pattern: pattern(f),
        choices: f.choices.iter().map(value_to_pb).collect(),
        secret: f.secret,
        hidden: f.hidden,
        derived: f.derived.as_ref().map_or_else(String::new, ToString::to_string),
        deliver: delivery_to_pb(f.deliver).into(),
        deliver_as: f.deliver_as.clone(),
        ..Default::default()
    }
}

/// The wire form of a recipe's configuration schema, its fields in the recipe's order.
#[must_use]
pub fn schema_to_pb(s: &ConfigSchema) -> pb::ConfigSchema {
    pb::ConfigSchema { fields: s.fields().iter().map(field_to_pb).collect(), ..Default::default() }
}
