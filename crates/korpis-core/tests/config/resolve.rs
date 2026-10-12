// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared configuration against the schema: what is written into the intent, what is refused,
//! and what the caller is changing.

use crate::support::{resources, schema};
use korpis_core::recipe::FieldPermission;
use korpis_core::value::{Value, ValueMap};

fn declared(pairs: &[(&str, Value)]) -> ValueMap {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn s(v: &str) -> Value {
    Value::String(v.into())
}

#[test]
fn defaults_constants_and_derived_values_are_written_into_the_intent() {
    let r = schema().resolve(&declared(&[("motd", s("hi"))]), None, &resources).unwrap();
    assert_eq!(r.values.get("max_players"), Some(&Value::Int(20)));
    assert_eq!(r.values.get("eula"), Some(&Value::Bool(true)));
    assert_eq!(r.values.get("java_heap"), Some(&Value::Int(3_435_973_836)));
    assert_eq!(r.values.get("rcon_password"), None, "no default, nothing declared");
    let keys: Vec<_> = r.changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["motd"], "only what the caller set differs from a new workload");
}

#[test]
fn invalid_values_never_reach_an_intent() {
    let sch = schema();
    let refuse =
        |pairs: &[(&str, Value)]| sch.resolve(&declared(pairs), None, &resources).unwrap_err();
    assert_eq!(refuse(&[("max_players", Value::Int(0))]).key, "max_players");
    assert_eq!(refuse(&[("max_players", s("20"))]).key, "max_players");
    assert_eq!(refuse(&[("motd", s("two\nlines"))]).key, "motd");
    assert_eq!(refuse(&[("motd", s(&"x".repeat(60)))]).key, "motd");
    assert_eq!(refuse(&[("jvm_flags", s("-Xmx1G; rm -rf /"))]).key, "jvm_flags");
    assert_eq!(refuse(&[("eula", Value::Bool(false))]).key, "eula");
    assert_eq!(refuse(&[("java_heap", Value::Int(1))]).key, "java_heap");
    assert_eq!(refuse(&[("unknown", Value::Int(1))]).key, "unknown");
    assert!(sch.resolve(&declared(&[("eula", Value::Bool(true))]), None, &resources).is_ok());
}

#[test]
fn changes_carry_the_permission_and_secrecy_the_caller_must_satisfy() {
    let sch = schema();
    let first = sch
        .resolve(&declared(&[("rcon_password", s("hunter2")), ("motd", s("a"))]), None, &resources)
        .unwrap();
    let rcon = first.changes.iter().find(|c| c.key == "rcon_password").unwrap();
    assert_eq!((rcon.permission, rcon.secret), (FieldPermission::Operator, true));

    // Re-declaring the operator's value unchanged is not a change, so a tenant may do it.
    let again = sch
        .resolve(
            &declared(&[("rcon_password", s("hunter2")), ("motd", s("b"))]),
            Some(&first.values),
            &resources,
        )
        .unwrap();
    let keys: Vec<_> = again.changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["motd"]);

    // Omitting it is a change back to no value, and needs the operator's authority.
    let dropped =
        sch.resolve(&declared(&[("motd", s("b"))]), Some(&first.values), &resources).unwrap();
    assert!(dropped.changes.iter().any(|c| c.key == "rcon_password"));
}
