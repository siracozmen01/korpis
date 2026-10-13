// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the runtime sets for a resolved configuration (§5.2 of docs/09-recipes.md).

use crate::support::{field, fields, resources, schema};
use korpis_core::recipe::deliver::text;
use korpis_core::recipe::{ConfigDelivery, ConfigSchema, ConfigType, FieldPermission};
use korpis_core::value::{Value, ValueMap};

fn s(v: &str) -> Value {
    Value::String(v.into())
}

#[test]
fn env_and_arg_fields_reach_the_runtime_in_declaration_order() {
    let declared: ValueMap = [("rcon_password", s("hunter2")), ("jvm_flags", s("-Xms1G"))]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
    let r = schema().resolve(&declared, None, &resources).unwrap();
    let d = schema().delivered(&r.values).unwrap();
    assert_eq!(d.env.get("RCON_PASSWORD").map(String::as_str), Some("hunter2"));
    assert_eq!(d.env.get("JAVA_HEAP").map(String::as_str), Some("3435973836"));
    assert_eq!(d.args, ["-Xms1G"], "a value is one argument, never split");
    assert_eq!(d.env.len(), 2, "template fields are not the runtime's");
    assert!(d.managed.is_empty());
}

#[test]
fn arguments_place_the_value_and_booleans_may_be_flags() {
    let mut port = field("port", ConfigType::Integer);
    (port.deliver, port.deliver_as) = (ConfigDelivery::Arg, "--port {}".into());
    let mut nogui = field("nogui", ConfigType::Boolean);
    (nogui.deliver, nogui.deliver_as) = (ConfigDelivery::Arg, "--nogui".into());
    let mut online = field("online", ConfigType::Boolean);
    (online.deliver, online.deliver_as) = (ConfigDelivery::Arg, "--online={}".into());
    let mut motd = field("motd", ConfigType::String);
    (motd.deliver, motd.deliver_as) = (ConfigDelivery::ManagedFile, String::new());
    let sch = ConfigSchema::new(vec![port, nogui, online, motd]).unwrap();
    let values = |nogui: bool| -> ValueMap {
        [("port", Value::Int(25565)), ("nogui", Value::Bool(nogui))]
            .into_iter()
            .chain([("online", Value::Bool(false)), ("motd", s("hi"))])
            .map(|(k, v)| (k.to_owned(), v))
            .collect()
    };
    let d = sch.delivered(&values(true)).unwrap();
    assert_eq!(d.args, ["--port", "25565", "--nogui", "--online=false"]);
    assert_eq!(d.managed, ["motd"], "named, so the caller delivers it or says it cannot");
    assert_eq!(sch.delivered(&values(false)).unwrap().args, ["--port", "25565", "--online=false"]);
    let mut none = values(false);
    none.insert("port".into(), Value::Null);
    assert_eq!(sch.delivered(&none).unwrap().args, ["--online=false"], "null sets nothing");
}

#[test]
fn values_are_written_in_one_form_whichever_was_declared() {
    assert_eq!(text(ConfigType::Quantity, &s("4Gi")).unwrap(), "4294967296");
    assert_eq!(text(ConfigType::Quantity, &s("500m")).unwrap(), "0.5");
    assert_eq!(text(ConfigType::Quantity, &Value::Int(7)).unwrap(), "7");
    assert_eq!(text(ConfigType::Duration, &s("1h30m")).unwrap(), "5400");
    assert_eq!(text(ConfigType::Duration, &s("1500ms")).unwrap(), "1.5");
    assert_eq!(text(ConfigType::Number, &Value::Double(0.25)).unwrap(), "0.25");
    assert_eq!(text(ConfigType::Enum, &s("hard")).unwrap(), "hard");
    assert!(text(ConfigType::String, &Value::List(vec![])).is_err());
}

#[test]
fn an_argument_must_place_its_value_and_a_variable_has_one_field() {
    let mut f = fields();
    f[4].deliver_as = "--flags".into();
    let m = ConfigSchema::new(f).unwrap_err().message;
    assert!(m.contains("{}"), "{m}");
    let mut f = fields();
    let mut twin = field("rcon_twin", ConfigType::String);
    twin.permission = FieldPermission::Tenant;
    (twin.deliver, twin.deliver_as) = (ConfigDelivery::Env, "RCON_PASSWORD".into());
    f.push(twin);
    let m = ConfigSchema::new(f).unwrap_err().message;
    assert!(m.contains("already delivers"), "{m}");
}
