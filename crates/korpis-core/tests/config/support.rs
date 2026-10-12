// SPDX-License-Identifier: AGPL-3.0-or-later

//! The schema of §5 of docs/09-recipes.md.

use korpis_core::recipe::config::anchored;
use korpis_core::recipe::derive::{Expr, Part, Resource};
use korpis_core::recipe::{ConfigDelivery, ConfigField, ConfigSchema, ConfigType, FieldPermission};
use korpis_core::value::Value;

/// A tenant string field delivered through templates, to be adjusted per test.
pub fn field(key: &str, ty: ConfigType) -> ConfigField {
    ConfigField {
        key: key.into(),
        ty,
        label: String::new(),
        permission: FieldPermission::Tenant,
        default: None,
        constant: None,
        min: None,
        max: None,
        max_length: None,
        pattern: None,
        choices: Vec::new(),
        secret: false,
        hidden: false,
        derived: None,
        deliver: ConfigDelivery::Template,
        deliver_as: String::new(),
    }
}

pub fn fields() -> Vec<ConfigField> {
    let mut max_players = field("max_players", ConfigType::Integer);
    (max_players.min, max_players.max) = (Some(1.0), Some(200.0));
    max_players.default = Some(Value::Int(20));

    let mut motd = field("motd", ConfigType::String);
    motd.max_length = Some(59);
    motd.pattern = Some(anchored(r"[^\r\n]*").unwrap());

    let mut rcon = field("rcon_password", ConfigType::String);
    (rcon.secret, rcon.permission) = (true, FieldPermission::Operator);
    (rcon.deliver, rcon.deliver_as) = (ConfigDelivery::Env, "RCON_PASSWORD".into());

    let mut heap = field("java_heap", ConfigType::Quantity);
    heap.permission = FieldPermission::System;
    heap.derived = Some(Expr::parse("resources.memory * 0.8").unwrap());
    (heap.deliver, heap.deliver_as) = (ConfigDelivery::Env, "JAVA_HEAP".into());

    let mut flags = field("jvm_flags", ConfigType::String);
    flags.permission = FieldPermission::Operator;
    flags.pattern = Some(anchored("[-A-Za-z0-9:+=./ ]*").unwrap());
    (flags.deliver, flags.deliver_as) = (ConfigDelivery::Arg, "{}".into());

    let mut eula = field("eula", ConfigType::Boolean);
    (eula.permission, eula.constant, eula.hidden) =
        (FieldPermission::System, Some(Value::Bool(true)), true);

    vec![max_players, motd, rcon, heap, flags, eula]
}

pub fn schema() -> ConfigSchema {
    ConfigSchema::new(fields()).expect("the documented example is a valid schema")
}

/// 4 GiB of memory, nothing else.
pub fn resources(r: Resource, _: Part) -> f64 {
    if r == Resource::Memory { 4_294_967_296.0 } else { 0.0 }
}
