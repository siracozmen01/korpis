// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a recipe's schema must satisfy when it is loaded.

use crate::support::{field, fields};
use korpis_core::recipe::derive::Expr;
use korpis_core::recipe::{ConfigDelivery, ConfigSchema, ConfigType, FieldPermission};
use korpis_core::value::Value;

fn refused(mutate: impl FnOnce(&mut Vec<korpis_core::recipe::ConfigField>)) -> String {
    let mut f = fields();
    mutate(&mut f);
    ConfigSchema::new(f).expect_err("refused").message
}

#[test]
fn protected_values_travel_only_by_channels_the_tenant_cannot_write() {
    let m = refused(|f| f[2].deliver = ConfigDelivery::Template);
    assert!(m.contains("tenant files"), "{m}");
    let m = refused(|f| f[4].deliver = ConfigDelivery::Template);
    assert!(m.contains("tenant files"), "{m}");
    let m = refused(|f| (f[2].deliver, f[2].deliver_as) = (ConfigDelivery::Arg, "{}".into()));
    assert!(m.contains("process table"), "{m}");
    let m = refused(|f| f[2].deliver_as = "1BAD".into());
    assert!(m.contains("deliver_as"), "{m}");
}

#[test]
fn system_fields_are_derived_or_fixed_and_derived_fields_are_system() {
    let m = refused(|f| f[3].permission = FieldPermission::Operator);
    assert!(m.contains("system-set"), "{m}");
    let m = refused(|f| f[5].constant = None);
    assert!(m.contains("derived or constant"), "{m}");
    let m = refused(|f| {
        let mut s = field("label", ConfigType::String);
        (s.permission, s.derived) = (FieldPermission::System, Some(Expr::parse("1").unwrap()));
        f.push(s);
    });
    assert!(m.contains("integer, number, or quantity"), "{m}");
}

#[test]
fn shapes_defaults_and_references_are_checked() {
    assert!(refused(|f| f.push(field("motd", ConfigType::String))).contains("twice"));
    assert!(refused(|f| f[0].key = "Max".into()).contains("keys are"));
    assert!(refused(|f| f[1].min = Some(1.0)).contains("numeric"));
    assert!(refused(|f| f[0].max_length = Some(3)).contains("strings only"));
    assert!(refused(|f| f.push(field("mode", ConfigType::Enum))).contains("choices"));
    assert!(refused(|f| f[0].default = Some(Value::Int(500))).contains("outside"));
    let m = refused(|f| f[3].derived = Some(Expr::parse("config.motd * 2").unwrap()));
    assert!(m.contains("plain number"), "{m}");
    let m = refused(|f| f[3].derived = Some(Expr::parse("config.nope").unwrap()));
    assert!(m.contains("not declared"), "{m}");
}
